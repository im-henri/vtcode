//! Vim mode through the real workbench keyboard pipeline.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::Mode;
use crate::app::{ActivePane, App, SidebarView, Tab};
use crate::editor::Editor;
use crate::explorer::{FileEntry, GitStatus};

fn app_with(text: &str) -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(text))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.terminal_has_focus = true;
    app.quick_suggestions = false;
    if let Some(ed) = app.editor_mut() {
        ed.viewport_height = 20;
        ed.viewport_width = 80;
    }
    app
}

fn vim_app(text: &str) -> App {
    let mut app = app_with(text);
    super::app_host::set_enabled(&mut app, true);
    app
}

fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, mods))
}

/// Type Vim notation through the workbench.
fn keys(app: &mut App, text: &str) {
    for key in super::keys::parse(text, &[]) {
        let ev = key.to_event();
        crate::input_keyboard::handle_keyboard_event(app, ev);
    }
}

fn text(app: &App) -> String {
    app.editor().unwrap().rope.to_string()
}
fn cursor(app: &App) -> (usize, usize) {
    let ed = app.editor().unwrap();
    (ed.cursor_line, ed.cursor_col)
}

#[test]
fn vim_mode_is_off_by_default_and_typing_inserts() {
    let mut app = app_with("abc");
    assert!(!app.vim.enabled);
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(text(&app), "jabc");
}

#[test]
fn normal_insert_round_trip_uses_native_typing() {
    let mut app = vim_app("hello world");
    keys(&mut app, "w");
    assert_eq!(cursor(&app), (0, 6));
    keys(&mut app, "A");
    assert_eq!(app.vim.mode, Mode::Insert);
    // Native typing: auto-pairs still apply in Insert mode.
    keys(&mut app, " (x");
    assert_eq!(text(&app), "hello world (x)");
    keys(&mut app, "<Esc>");
    assert_eq!(app.vim.mode, Mode::Normal);
    assert_eq!(
        cursor(&app),
        (0, 13),
        "Esc moves left onto the last typed character"
    );
    keys(&mut app, "u");
    assert_eq!(
        text(&app),
        "hello world",
        "one undo step for the whole insert"
    );
}

#[test]
fn normal_mode_keys_do_not_type_and_ctrl_keys_follow_vim() {
    let mut app = vim_app(&"line\n".repeat(60));
    keys(&mut app, "x");
    assert_eq!(&text(&app)[..4], "ine\n");
    // <C-d> scrolls half a screen instead of the VS Code "select next".
    keys(&mut app, "<C-d>");
    assert_eq!(cursor(&app).0, 10);
    assert!(app.editor().unwrap().extra_cursors.is_empty());
    // <C-v> starts Visual block mode instead of pasting.
    keys(&mut app, "<C-v>");
    assert_eq!(app.vim.mode, Mode::Visual(super::VisualKind::Block));
    keys(&mut app, "<Esc>");
    assert_eq!(app.vim.mode, Mode::Normal);
    // Ctrl+P is not a Normal-mode Vim command: Quick Open still works.
    keys(&mut app, "<C-p>");
    assert!(app.quick_open_active);
}

#[test]
fn ex_commands_reach_the_workbench() {
    let mut app = vim_app("abc");
    let before = app.editor().unwrap().word_wrap;
    keys(&mut app, ":VT toggle_word_wrap<CR>");
    assert_ne!(app.editor().unwrap().word_wrap, before);
    keys(&mut app, ":set rnu<CR>");
    assert!(app.relative_line_numbers);
    keys(&mut app, ":set nornu<CR>");
    assert!(!app.relative_line_numbers);
    keys(&mut app, ":nosuchcommand<CR>");
    assert!(
        app.vim
            .message
            .as_ref()
            .is_some_and(|m| m.error && m.text.starts_with("E492"))
    );
}

#[test]
fn quit_with_unsaved_changes_reports_e37() {
    let mut app = vim_app("abc");
    keys(&mut app, "x:q<CR>");
    assert!(
        app.vim
            .message
            .as_ref()
            .is_some_and(|m| m.text.starts_with("E37"))
    );
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn visual_selection_is_mirrored_for_rendering_and_commands() {
    let mut app = vim_app("one two\nthree");
    keys(&mut app, "vl");
    assert_eq!(app.editor().unwrap().selection_range(), Some((0, 0, 0, 2)));
    keys(&mut app, "<Esc>V");
    assert_eq!(app.editor().unwrap().selection_range(), Some((0, 0, 1, 0)));
    keys(&mut app, "<Esc>");
    assert!(!app.editor().unwrap().has_selection());
}

fn with_explorer(app: &mut App) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    app.explorer.root = root.path().into();
    app.explorer.entries = (0..5)
        .map(|i| {
            let name = format!("f{i}.txt");
            FileEntry {
                path: root.path().join(&name),
                name,
                is_dir: false,
                depth: 0,
                expanded: false,
                git_status: GitStatus::Unmodified,
                git_status_summary: Default::default(),
            }
        })
        .collect();
    app.show_explorer = true;
    app.sidebar_view = SidebarView::Explorer;
    root
}

#[test]
fn explorer_moves_with_j_and_k() {
    let mut app = vim_app("abc");
    let _root = with_explorer(&mut app);
    app.active_pane = ActivePane::Explorer;
    app.explorer.selected = 0;
    keys(&mut app, "jj");
    assert_eq!(app.explorer.selected, 2);
    keys(&mut app, "k");
    assert_eq!(app.explorer.selected, 1);
    keys(&mut app, "G");
    assert_eq!(app.explorer.selected, 4);
    keys(&mut app, "gg");
    assert_eq!(app.explorer.selected, 0);
    keys(&mut app, "q");
    assert_eq!(app.active_pane, ActivePane::Editor);
}

#[test]
fn ctrl_w_moves_focus_between_explorer_and_editor() {
    let mut app = vim_app("abc");
    let _root = with_explorer(&mut app);
    app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 20, 30));
    app.editor_area = Some(ratatui::layout::Rect::new(20, 0, 60, 30));
    keys(&mut app, "<C-w>h");
    assert_eq!(app.active_pane, ActivePane::Explorer);
    keys(&mut app, "<C-w>l");
    assert_eq!(app.active_pane, ActivePane::Editor);
}

#[test]
fn toggling_the_setting_resets_editor_state() {
    let mut app = app_with("abc");
    app.toggle_setting("vim_mode");
    assert!(app.vim.enabled);
    keys(&mut app, "v");
    assert!(app.editor().unwrap().vim_selection.is_some());
    app.toggle_setting("vim_mode");
    assert!(!app.vim.enabled);
    assert!(app.editor().unwrap().vim_selection.is_none());
    press(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(text(&app), "zabc");
}

#[test]
fn switching_tabs_leaves_insert_mode() {
    let mut app = vim_app("abc");
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("second")));
    keys(&mut app, "i");
    assert_eq!(app.vim.mode, Mode::Insert);
    super::app_host::activate_tab(&mut app, 1);
    keys(&mut app, "l");
    assert_eq!(app.vim.mode, Mode::Normal);
    assert_eq!(text(&app), "second", "the key is a motion, not typed text");
    assert_eq!(cursor(&app), (0, 1));
}

#[test]
fn status_item_names_the_mode() {
    let mut app = vim_app("abc");
    assert!(super::ui::status_item(&app).unwrap().0.contains("NORMAL"));
    keys(&mut app, "i");
    assert!(super::ui::status_item(&app).unwrap().0.contains("INSERT"));
    keys(&mut app, "<Esc>qa");
    assert!(
        super::ui::status_item(&app)
            .unwrap()
            .0
            .contains("recording @a")
    );
    keys(&mut app, "q2d");
    assert!(super::ui::status_item(&app).unwrap().0.contains("2d"));
}

fn frame(app: &mut App) -> (Vec<String>, Option<ratatui::layout::Position>) {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    let buf = terminal.backend().buffer();
    let rows = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        })
        .collect();
    let cursor = ratatui::backend::Backend::get_cursor_position(terminal.backend_mut()).ok();
    (rows, app.vim_cursor_shape.map(|_| cursor.unwrap()))
}

#[test]
fn full_frame_shows_mode_command_line_and_insert_caret() {
    let mut app = vim_app("fn main() {\n    println!(\"hi\");\n}\n");
    let (rows, caret) = frame(&mut app);
    let status = rows.last().unwrap().clone();
    assert!(status.contains("NORMAL"), "status bar: {status}");
    assert!(caret.is_none(), "Normal mode paints its own block caret");

    keys(&mut app, ":s/ma");
    let (rows, _) = frame(&mut app);
    assert!(
        rows.last().unwrap().trim_start().starts_with(":s/ma"),
        "command line: {}",
        rows.last().unwrap()
    );
    keys(&mut app, "<Esc>");

    keys(&mut app, "jwi");
    let (rows, caret) = frame(&mut app);
    assert!(rows.last().unwrap().contains("INSERT"));
    let caret = caret.expect("Insert mode shows the terminal bar cursor");
    // The caret sits on the p of println.
    assert_eq!(
        rows[caret.y as usize].chars().nth(caret.x as usize),
        Some('p')
    );
    assert_eq!(
        app.vim_cursor_shape,
        Some(crossterm::cursor::SetCursorStyle::BlinkingBar)
    );

    keys(&mut app, "<Esc>:nosuch<CR>");
    let (rows, _) = frame(&mut app);
    assert!(
        rows.last().unwrap().contains("E492"),
        "error message: {}",
        rows.last().unwrap()
    );

    keys(&mut app, ":reg<CR>");
    let (rows, _) = frame(&mut app);
    assert!(rows.iter().any(|r| r.contains("Press ENTER")));
}

#[test]
fn ctrl_backslash_ctrl_n_leaves_a_focused_terminal() {
    let mut app = vim_app("abc");
    let mut pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "");
    let input = pty.test_capture_input();
    app.terminals[0].terminal_pty = Some(pty);
    app.terminals[0].dock = crate::app::TerminalDock::Right;
    app.show_terminal_sidebar = true;
    app.terminal_panel_focused = true;
    assert!(app.focused_terminal_index().is_some());
    // Ordinary keys reach the shell, Ctrl-W included (shells use it).
    keys(&mut app, "ls<C-w>");
    assert_eq!(&*input.lock().unwrap(), b"ls\x17");
    // Both spellings terminals use for Ctrl-\ work.
    for backslash in [KeyCode::Char('\\'), KeyCode::Char('4')] {
        app.terminal_panel_focused = true;
        press(&mut app, backslash, KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(!app.terminal_panel_focused, "focus returns to the editor");
        assert_eq!(app.active_pane, ActivePane::Editor);
    }
    keys(&mut app, "x");
    assert_eq!(text(&app), "bc", "Vim keys act on the editor again");
}

fn with_problems(app: &mut App) {
    let id = app.editor().unwrap().navigation_id;
    app.cached_problems = (1..=3)
        .map(|line| {
            (
                "a.rs".to_string(),
                line * 2,
                "TODO".to_string(),
                format!("item {line}"),
                id,
            )
        })
        .collect();
    app.panel_area = Some(ratatui::layout::Rect::new(0, 20, 80, 8));
    app.editor_area = Some(ratatui::layout::Rect::new(0, 0, 80, 20));
}

#[test]
fn copen_focuses_problems_and_j_enter_jump_to_the_location() {
    let mut app = vim_app(&"line\n".repeat(10));
    with_problems(&mut app);
    keys(&mut app, ":copen<CR>");
    assert!(app.show_panel && app.panel_tab == 1);
    // Focus survives a drawn frame (the panel used to hand it back).
    frame(&mut app);
    assert_eq!(app.active_pane, ActivePane::Panel);
    with_problems(&mut app);
    keys(&mut app, "jj");
    assert_eq!(
        app.problem_selected, 3,
        "the third problem row (after the summary)"
    );
    keys(&mut app, "k");
    assert_eq!(app.problem_selected, 2);
    keys(&mut app, "G");
    assert_eq!(app.problem_selected, 3);
    keys(&mut app, "ggl");
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert_eq!(cursor(&app), (1, 0), "TODO on line 2 opens at its line");
    // Ctrl-W j goes back down to the panel, q returns, :cclose hides it.
    keys(&mut app, "<C-w>j");
    assert_eq!(app.active_pane, ActivePane::Panel);
    assert!(!app.terminal_panel_focused);
    keys(&mut app, "q");
    assert_eq!(app.active_pane, ActivePane::Editor);
    keys(&mut app, ":ccl<CR>");
    assert!(!app.show_panel);
}

#[test]
fn problems_panel_works_from_the_keyboard_without_vim_mode() {
    let mut app = app_with(&"line\n".repeat(10));
    with_problems(&mut app);
    crate::ui_panels::show_problems(&mut app);
    app.active_pane = ActivePane::Panel;
    press(&mut app, KeyCode::End, KeyModifiers::NONE);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert_eq!(cursor(&app), (5, 0));
}

#[test]
fn tab_context_menu_moves_with_j_and_k_and_q_closes() {
    let mut app = vim_app("abc");
    app.open_tab_text_color_menu(0);
    let start = app.tab_context_selected;
    keys(&mut app, "j");
    assert_eq!(app.tab_context_selected, start + 1);
    keys(&mut app, "k");
    assert_eq!(app.tab_context_selected, start);
    keys(&mut app, "q");
    assert!(!app.tab_context_active);
    assert_eq!(text(&app), "abc", "menu keys never reach the buffer");
}

#[test]
fn dialog_buttons_move_with_h_and_l() {
    let mut app = vim_app("abc");
    keys(&mut app, "x");
    app.close_tab(0);
    assert!(app.confirm_close_active);
    let start = app.modal.focus;
    keys(&mut app, "l");
    assert_ne!(app.modal.focus, start);
    keys(&mut app, "h");
    assert_eq!(app.modal.focus, start);
    keys(&mut app, "<Esc>");
    assert!(!app.confirm_close_active);
    assert_eq!(text(&app), "bc");
}

#[test]
fn agent_chat_leaves_with_ctrl_backslash_ctrl_n_and_ctrl_w() {
    let mut app = vim_app("abc");
    app.chat.visible = true;
    app.active_pane = ActivePane::Chat;
    keys(&mut app, r"<C-\><C-n>");
    assert_eq!(app.active_pane, ActivePane::Editor);
    app.active_pane = ActivePane::Chat;
    keys(&mut app, "<C-w>c");
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert!(
        !app.chat.layout.dock_visible || !app.chat.visible,
        "Ctrl-W c hides the docked chat"
    );
    assert_eq!(app.tabs.len(), 1, "the file tab stays open");
    keys(&mut app, "x");
    assert_eq!(text(&app), "bc");
}

#[test]
fn commands_that_reveal_a_sidebar_move_focus_there() {
    let mut app = vim_app("abc");
    let _root = with_explorer(&mut app);
    app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 20, 30));
    app.editor_area = Some(ratatui::layout::Rect::new(20, 0, 60, 30));
    keys(&mut app, ":VT source_control<CR>");
    assert_eq!(app.sidebar_view, SidebarView::SourceControl);
    assert_eq!(
        app.active_pane,
        ActivePane::Explorer,
        "Source Control takes the keyboard"
    );
    keys(&mut app, "q");
    assert_eq!(app.active_pane, ActivePane::Editor);
    // Ctrl-W h returns to the view that is showing, not the file tree.
    keys(&mut app, "<C-w>h");
    assert_eq!(app.sidebar_view, SidebarView::SourceControl);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    keys(&mut app, "<C-w>l");
    assert_eq!(app.active_pane, ActivePane::Editor);
    keys(&mut app, ":VT extensions<CR>");
    assert!(
        app.vim.message.is_none(),
        "{:?}",
        app.vim.message.as_ref().map(|m| &m.text)
    );
    assert_eq!(app.sidebar_view, SidebarView::Extensions);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    // Focus starts in the filter; Esc leaves it for the list, q for the editor.
    keys(&mut app, "<Esc>");
    assert!(!app.ext_sidebar.search_focused);
    keys(&mut app, "q");
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert_eq!(text(&app), "abc");
}

#[test]
fn text_fields_take_ctrl_w_and_ctrl_u_like_the_vim_command_line() {
    let mut app = vim_app("abc");
    keys(&mut app, "<C-p>");
    assert!(app.quick_open_active);
    keys(&mut app, "src main");
    keys(&mut app, "<C-w>");
    assert_eq!(app.quick_open_query, "src ");
    keys(&mut app, "<C-u>");
    assert_eq!(app.quick_open_query, "");
    assert!(app.quick_open_active, "the field keeps focus");
    assert_eq!(text(&app), "abc");
}

#[test]
fn settings_and_settings_dialogs_close_with_esc() {
    let mut app = vim_app("abc");
    let settings_open = |app: &App| {
        app.tabs
            .iter()
            .any(|t| t.kind == crate::app::TabKind::Settings)
    };
    keys(&mut app, ":VT settings<CR>");
    assert!(settings_open(&app));
    keys(&mut app, "<Esc>");
    assert!(!settings_open(&app), "Esc closes Settings");
    // Agent settings open a section of Settings: Esc steps out of the
    // section, a second Esc closes Settings.
    keys(&mut app, ":VT acp_configure<CR>");
    assert!(settings_open(&app));
    keys(&mut app, "<Esc><Esc>");
    assert!(!settings_open(&app));
    // Formatter settings are a dialog.
    keys(&mut app, ":VT configure_formatter<CR>");
    assert!(
        crate::ui_modal::active(&app).is_some(),
        "the formatter dialog opens"
    );
    keys(&mut app, "<Esc>");
    assert!(crate::ui_modal::active(&app).is_none(), "Esc closes it");
    assert_eq!(
        app.active_pane,
        ActivePane::Editor,
        "focus is back in the editor"
    );
    keys(&mut app, "x");
    assert_eq!(text(&app), "bc");
}

#[test]
fn vt_accepts_shortcut_ids_for_commands() {
    let mut app = vim_app(
        "one
two
three",
    );
    keys(&mut app, ":VT go_to_line<CR>");
    assert!(
        app.vim.message.is_none(),
        "{:?}",
        app.vim.message.as_ref().map(|m| &m.text)
    );
    assert!(app.goto_line_active);
    keys(&mut app, "3<CR>");
    assert_eq!(cursor(&app).0, 2);
}

#[test]
fn colon_opens_the_command_line_from_sidebars_and_panels() {
    let mut app = vim_app("abc");
    let _root = with_explorer(&mut app);
    app.active_pane = ActivePane::Explorer;
    keys(&mut app, ":s/b/B/<CR>");
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert_eq!(text(&app), "aBc", "the Ex command ran on the editor");
    with_problems(&mut app);
    keys(&mut app, ":copen<CR>");
    assert_eq!(app.active_pane, ActivePane::Panel);
    keys(&mut app, ":cclose<CR>");
    assert!(!app.show_panel);
    assert_eq!(app.active_pane, ActivePane::Editor);
}

fn mouse_at(app: &mut App, kind: crossterm::event::MouseEventKind, x: u16, y: u16) {
    crate::mouse::handle_mouse(
        app,
        crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn a_mouse_drag_selects_the_dragged_characters_in_visual_mode() {
    use crossterm::event::{MouseButton, MouseEventKind as M};
    let mut app = vim_app("alpha\ngamma\n");
    let (rows, _) = frame(&mut app);
    let y = rows.iter().position(|r| r.contains("gamma")).unwrap() as u16;
    let row = &rows[y as usize];
    let x = row[..row.find("gamma").unwrap()].chars().count() as u16;
    mouse_at(&mut app, M::Down(MouseButton::Left), x, y);
    for dx in [0, 1, 1, 2] {
        mouse_at(&mut app, M::Drag(MouseButton::Left), x + dx, y);
    }
    mouse_at(&mut app, M::Up(MouseButton::Left), x + 2, y);
    assert_eq!(app.vim.mode, Mode::Visual(super::VisualKind::Char));
    keys(&mut app, "d");
    assert_eq!(
        text(&app),
        "alpha\nma\n",
        "Visual mode includes the character under the pointer, as in Vim"
    );
    // Dragging leftwards includes both ends too.
    keys(&mut app, "u");
    mouse_at(&mut app, M::Down(MouseButton::Left), x + 4, y);
    for dx in [3, 2] {
        mouse_at(&mut app, M::Drag(MouseButton::Left), x + dx, y);
    }
    mouse_at(&mut app, M::Up(MouseButton::Left), x + 2, y);
    keys(&mut app, "d");
    assert_eq!(text(&app), "alpha\nga\n");
    // A plain click afterwards is Normal mode with no selection.
    mouse_at(&mut app, M::Down(MouseButton::Left), x, y);
    mouse_at(&mut app, M::Up(MouseButton::Left), x, y);
    assert_eq!(app.vim.mode, Mode::Normal);
}

#[test]
fn cd_changes_where_relative_ex_paths_start() {
    let mut app = vim_app("abc");
    let root = tempfile::tempdir().unwrap();
    let root_path = crate::portable::filesystem_spelling(&root.path().canonicalize().unwrap());
    std::fs::create_dir(root_path.join("sub")).unwrap();
    app.explorer.root = root_path.clone();
    keys(&mut app, ":cd sub<CR>");
    assert!(
        app.vim.message.is_none(),
        "{:?}",
        app.vim.message.as_ref().map(|m| &m.text)
    );
    assert_eq!(
        super::app_host::vim_cwd(&app),
        root_path.join("sub").as_path()
    );
    keys(&mut app, ":pwd<CR>");
    assert!(
        app.vim
            .message
            .as_ref()
            .is_some_and(|m| m.text.ends_with("sub"))
    );
    keys(&mut app, ":cd -<CR>");
    keys(&mut app, ":pwd<CR>");
    assert_eq!(
        app.vim.message.as_ref().map(|m| m.text.clone()),
        Some(root_path.display().to_string())
    );
    keys(&mut app, ":cd nowhere<CR>");
    assert!(
        app.vim
            .message
            .as_ref()
            .is_some_and(|m| m.error && m.text.starts_with("E344"))
    );
    // Opening another folder starts from that folder again.
    keys(&mut app, ":cd sub<CR>");
    app.explorer.root = std::env::temp_dir();
    assert_eq!(
        super::app_host::vim_cwd(&app),
        std::env::temp_dir().as_path()
    );
}

/// Pretend the current prefix has waited long enough for its hints.
fn wait_for_hints(app: &mut App) {
    frame(app);
    if let Some((_, since)) = app.vim_hint_since.as_mut() {
        *since -= super::feedback::HINT_DELAY * 2;
    }
}

fn screen_text(app: &mut App) -> String {
    frame(app).0.join("\n")
}

#[test]
fn the_badge_names_the_focused_pane() {
    let mut app = vim_app("abc");
    let _root = with_explorer(&mut app);
    assert!(frame(&mut app).0.last().unwrap().contains("NORMAL"));
    app.active_pane = ActivePane::Explorer;
    assert!(frame(&mut app).0.last().unwrap().contains("EXPLORER"));
    with_problems(&mut app);
    keys(&mut app, ":copen<CR>");
    assert!(frame(&mut app).0.last().unwrap().contains("PROBLEMS"));
    keys(&mut app, "q");
    assert!(frame(&mut app).0.last().unwrap().contains("NORMAL"));
}

#[test]
fn ctrl_w_shows_as_pending_and_then_lists_window_commands() {
    let mut app = vim_app("abc");
    let _root = with_explorer(&mut app);
    app.active_pane = ActivePane::Explorer;
    keys(&mut app, "<C-w>");
    let (rows, _) = frame(&mut app);
    assert!(
        rows.last().unwrap().contains("^W"),
        "pending ^W in the status bar: {}",
        rows.last().unwrap()
    );
    assert!(
        !screen_text(&mut app).contains("move left / down"),
        "no popup before the pause"
    );
    wait_for_hints(&mut app);
    let screen = screen_text(&mut app);
    assert!(screen.contains("^W window"), "{screen}");
    assert!(screen.contains("move left / down / up / right"));
    assert!(screen.contains("split vertically"));
    keys(&mut app, "l");
    assert!(
        !screen_text(&mut app).contains("move left / down"),
        "the popup closes with the command"
    );
    assert_eq!(app.active_pane, ActivePane::Editor);
}

#[test]
fn editor_prefixes_list_what_can_follow() {
    let mut app = vim_app("abc def");
    keys(&mut app, "g");
    assert!(frame(&mut app).0.last().unwrap().contains(" g"));
    wait_for_hints(&mut app);
    let screen = screen_text(&mut app);
    assert!(screen.contains("definition / declaration"), "{screen}");
    keys(&mut app, "<Esc>d");
    wait_for_hints(&mut app);
    assert!(screen_text(&mut app).contains("whole line (dd, yy, cc...)"));
    keys(&mut app, "i");
    wait_for_hints(&mut app);
    assert!(screen_text(&mut app).contains("text object"));
    keys(&mut app, "w");
    assert_eq!(text(&app), " def");
    // :set nowhichkey turns the popup off; the pending keys still show.
    keys(&mut app, ":set nowhichkey<CR>g");
    wait_for_hints(&mut app);
    let (rows, _) = frame(&mut app);
    assert!(!rows.join("\n").contains("definition / declaration"));
    assert!(rows.last().unwrap().contains(" g"));
}

#[test]
fn keys_that_are_not_commands_say_so() {
    let mut app = vim_app("abc");
    keys(&mut app, "<C-w>Z");
    let message = app
        .vim
        .message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(message.contains("not a window command"), "{message}");
    assert!(super::feedback::bell_active(&app), "the badge flashes");
    keys(&mut app, "gX");
    let message = app
        .vim
        .message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert_eq!(message, "gX: not a command");
    assert!(
        frame(&mut app)
            .0
            .last()
            .unwrap()
            .contains("gX: not a command")
    );
    // The message goes with the next key; editing is unaffected.
    keys(&mut app, "x");
    assert!(app.vim.message.is_none());
    assert_eq!(text(&app), "bc");
    // The same from a sidebar.
    let _root = with_explorer(&mut app);
    app.active_pane = ActivePane::Explorer;
    keys(&mut app, "<C-w>Z");
    assert!(
        app.vim
            .message
            .as_ref()
            .is_some_and(|m| m.text.contains("not a window command"))
    );
}

#[test]
fn insert_mode_prefixes_and_one_shot_normal_commands_are_shown() {
    let mut app = vim_app("abc");
    keys(&mut app, "i<C-r>");
    assert!(frame(&mut app).0.last().unwrap().contains("^R"));
    wait_for_hints(&mut app);
    assert!(screen_text(&mut app).contains("insert register"));
    keys(&mut app, "<Esc><C-k>");
    assert!(frame(&mut app).0.last().unwrap().contains("^K"));
    keys(&mut app, "a:");
    assert_eq!(text(&app), "äabc");
    // <C-o> runs one Normal-mode command: Vim shows "(insert)".
    keys(&mut app, "<C-o>");
    let status = frame(&mut app).0.last().unwrap().clone();
    assert!(
        status.contains("(INSERT)"),
        "status: {status} mode={:?} oneshot={}",
        app.vim.mode,
        app.vim.insert_oneshot.is_some()
    );
    keys(&mut app, "$");
    assert!(frame(&mut app).0.last().unwrap().contains("INSERT"));
    assert_eq!(app.vim.mode, Mode::Insert);
}

#[test]
fn a_window_move_outlines_the_pane_it_lands_in() {
    let mut app = vim_app("abc");
    let _root = with_explorer(&mut app);
    frame(&mut app);
    keys(&mut app, "<C-w>h");
    assert_eq!(app.active_pane, ActivePane::Explorer);
    let (_, area) = app.vim_focus_flash.expect("the Explorer is outlined");
    assert_eq!(Some(area), app.explorer_area);
    assert!(
        super::feedback::next_deadline(&app).is_some(),
        "a redraw is scheduled to clear it"
    );
}

#[test]
fn ctrl_o_in_insert_mode_runs_one_normal_command() {
    let mut app = vim_app("abc def");
    keys(&mut app, "i<C-o>");
    assert_eq!(
        app.vim.mode,
        Mode::Normal,
        "<C-o> waits for one Normal-mode command"
    );
    keys(&mut app, "w");
    assert_eq!(app.vim.mode, Mode::Insert, "then Insert mode resumes");
    keys(&mut app, "X<Esc>");
    assert_eq!(text(&app), "abc Xdef");
    // A multi-key command counts as one: <C-o>dw.
    keys(&mut app, "0i<C-o>dw");
    assert_eq!(app.vim.mode, Mode::Insert);
    assert_eq!(text(&app), "Xdef");
}

#[test]
fn visual_mode_shows_the_selection_size_and_search_shows_its_count() {
    let mut app = vim_app("one two\nthree four\nfive\n");
    let status = |app: &mut App| frame(app).0.last().unwrap().clone();
    keys(&mut app, "vll");
    assert!(
        status(&mut app).contains("VISUAL  3"),
        "{}",
        status(&mut app)
    );
    keys(&mut app, "j");
    assert!(status(&mut app).contains("VISUAL  2"));
    keys(&mut app, "<Esc>Vj");
    assert!(status(&mut app).contains("V-LINE  2"));
    keys(&mut app, "<Esc>gg<C-v>jl");
    assert!(
        status(&mut app).contains("V-BLOCK  2x2"),
        "{}",
        status(&mut app)
    );
    keys(&mut app, "<Esc>gg/o<CR>");
    let message = app
        .vim
        .message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert_eq!(message, "/o  [2/3]", "o in one, two, four: the second");
    keys(&mut app, "nn");
    let message = app
        .vim
        .message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert_eq!(message, "search hit BOTTOM, continuing at TOP  [1/3]");
}

#[test]
fn lists_say_which_keys_they_take() {
    let mut app = vim_app(&"line\n".repeat(10));
    with_problems(&mut app);
    keys(&mut app, ":copen<CR>x");
    let message = app
        .vim
        .message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        message.starts_with("x: not a key here (j k gg G move"),
        "{message}"
    );
    assert!(super::feedback::bell_active(&app));
    assert_eq!(app.active_pane, ActivePane::Panel, "focus stays");
    keys(&mut app, "q");
    let _root = with_explorer(&mut app);
    app.active_pane = ActivePane::Explorer;
    keys(&mut app, "gX");
    assert!(
        app.vim
            .message
            .as_ref()
            .is_some_and(|m| m.text.starts_with("gX: not a key here"))
    );
    keys(&mut app, "j");
    assert!(app.vim.message.is_none(), "the next key clears it");
}

#[test]
fn sidebar_views_switch_from_any_sidebar_view() {
    let mut app = vim_app("abc");
    let _root = with_explorer(&mut app);
    app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 20, 30));
    app.editor_area = Some(ratatui::layout::Rect::new(20, 0, 60, 30));
    // Into Source Control from the editor, then back to the Explorer.
    keys(&mut app, ":VT source_control<CR>");
    assert_eq!(app.sidebar_view, SidebarView::SourceControl);
    app.active_pane = ActivePane::Explorer; // focused, as the app does
    assert_eq!(super::feedback::badge(&app), "SOURCE CONTROL");
    keys(&mut app, ":Explore<CR>");
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    // <C-w>l returns to the editor; <C-w>h reaches whichever view the sidebar shows.
    keys(&mut app, ":VT source_control<CR><C-w>l");
    assert_eq!(app.active_pane, ActivePane::Editor);
    keys(&mut app, "<C-w>h");
    assert_eq!(app.sidebar_view, SidebarView::SourceControl);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    // The usual VTCode shortcut works from a sidebar view too.
    press(
        &mut app,
        KeyCode::Char('E'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    press(
        &mut app,
        KeyCode::Char('G'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(app.sidebar_view, SidebarView::SourceControl);
    keys(&mut app, ":VT global_search<CR>");
    assert_eq!(app.sidebar_view, SidebarView::Search);
    keys(&mut app, "<Esc>:VT extensions<CR>");
    assert_eq!(app.sidebar_view, SidebarView::Extensions);
}

#[test]
fn the_command_line_opens_from_a_sidebar_with_no_file_open() {
    let mut app = vim_app("");
    let _root = with_explorer(&mut app);
    app.tabs.clear();
    keys(&mut app, ":");
    keys(&mut app, "VT source_control<CR>");
    assert_eq!(app.sidebar_view, SidebarView::SourceControl);
    app.active_pane = ActivePane::Explorer;
    keys(&mut app, ":");
    assert!(app.vim.cmdline.is_some(), "':' opens the command line");
    assert!(
        screen_text(&mut app)
            .lines()
            .last()
            .is_some_and(|l| l.starts_with(":")),
        "it shows at the bottom"
    );
    keys(&mut app, "Explore<CR>");
    assert!(app.vim.cmdline.is_none());
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    // A command that needs a file says so instead of doing nothing.
    keys(&mut app, ":w<CR>");
    assert!(app.vim.cmdline.is_none());
    assert!(app.vim.message.is_some(), "an error is shown");
    // <Esc> closes an unfinished command line.
    keys(&mut app, ":Exp<Esc>");
    assert!(app.vim.cmdline.is_none());
    assert_eq!(app.active_pane, ActivePane::Explorer);
}

#[test]
fn x_and_zz_close_an_unchanged_buffer_without_a_name() {
    // Vim writes only a changed buffer: on the last, unchanged tab they quit.
    for cmd in [":x<CR>", "ZZ"] {
        let mut app = vim_app("abc");
        let mut quit = false;
        for key in super::keys::parse(cmd, &[]) {
            quit |= crate::input_keyboard::handle_keyboard_event(&mut app, key.to_event());
        }
        assert!(quit, "{cmd} quits");
    }
    // A changed one still needs a name.
    let mut app = vim_app("abc");
    keys(&mut app, "x:x<CR>");
    assert!(
        app.vim
            .message
            .as_ref()
            .is_some_and(|m| m.text.starts_with("E32"))
    );
}
