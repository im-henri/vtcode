//! Workbench integration: [`Host`] for [`App`], keyboard routing, window
//! navigation and keyboard-only control of the sidebars and popups.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::keys::{K, Key};
use super::parse::Window;
use super::text::{Pos, Text};
use super::{Act, Host, Mode, VimState, exec};
use crate::app::{ActivePane, App, TabKind};
use crate::editor::Editor;

pub(crate) struct AppHost<'a> {
    pub(crate) app: &'a mut App,
}

/// The active tab is a text editor Vim may drive.
pub(crate) fn vim_tab(app: &App) -> bool {
    app.tabs
        .get(app.active_tab)
        .is_some_and(|tab| matches!(tab.kind, TabKind::File | TabKind::GitRevision))
        && app.active_extension_view().is_none()
        && app
            .editor()
            .is_some_and(|ed| ed.markdown_preview_scroll.is_none())
}

/// Popups that own the keyboard while open.
pub(crate) fn popup_open(app: &App) -> bool {
    app.palette.active
        || app.quick_open_active
        || app.goto_line_active
        || app.symbol_list_active
        || app.explorer_context_active
        || app.context_menu_active
        || app.hunk_popup_active
        || app.confirm_close_active
        || app.exit_confirm_active
        || app.sc_context_active
        || app.tab_context_active
        || app.shortcuts_active
        || (app.lsp_code_action_active && !app.lsp_manager.code_actions.is_empty())
        || app.call_hierarchy_active
        || app.type_hierarchy_active
        || (app.lsp_references_active && !app.lsp_manager.reference_locations.is_empty())
        || crate::ui_modal::active(app).is_some()
        || crate::ui_quick_picker::active(app).is_some()
        || app.change_dir_active
        || app.menu_active
        || app.menu_open
        || app.desks.popup.mode.is_some()
        || app.workspaces.popup.open
}

/// Whether Vim handles keys for the focused editor.
pub(crate) fn owns_editor(app: &App) -> bool {
    app.vim.enabled
        && app.active_pane == ActivePane::Editor
        && !app.terminal_panel_focused
        && app.focused_terminal_index().is_none()
        && app.focused_text_input_id().is_none()
        && !app.settings_editor_has_focus()
        && !app.diff_selection_owns_cursor()
        && vim_tab(app)
        && !popup_open(app)
}

fn completion_visible(app: &App) -> bool {
    app.vtcomplete_active
        || crate::ui_lsp_completion::visible(app)
        || app.inline_ghost_visible()
        || app.lsp_code_action_active && !app.lsp_manager.code_actions.is_empty()
}

impl Host for AppHost<'_> {
    fn ed(&mut self) -> Option<&mut Editor> {
        if !vim_tab(self.app) {
            return None;
        }
        self.app.editor_mut()
    }

    fn ed_ref(&self) -> Option<&Editor> {
        if !vim_tab(self.app) {
            return None;
        }
        self.app.editor()
    }

    fn native_key(&mut self, key: Key) {
        let event = key.to_event();
        let was = self.app.vim_bypass;
        self.app.vim_bypass = true;
        let quit = crate::input_keyboard::handle_keyboard_event_inner(self.app, event);
        self.app.vim_bypass = was;
        if quit {
            self.app.vim_quit = true;
        }
    }

    fn open_line(&mut self, above: bool, autoindent: bool) {
        let Some(ed) = self.ed() else {
            return;
        };
        // The editor's own indentation rules (indent after `{`, before `}`).
        if above {
            ed.insert_line_above();
        } else {
            ed.insert_line_below();
        }
        if !autoindent {
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let line = ed.cursor_line;
            let ws = t
                .line_chars(line)
                .iter()
                .take_while(|&&c| c == ' ' || c == '\t')
                .count();
            if ws > 0 && ws == t.len(line) {
                let s = t.line_start(line);
                let mut tx = exec::Tx::default();
                tx.edit(ed, s, s + ws, "");
                tx.finish(ed);
                ed.cursor_col = 0;
            }
        }
    }

    fn clip_get(&mut self) -> Option<String> {
        crate::clipboard::get_text().ok().or_else(|| {
            self.app
                .editor()
                .map(|ed| ed.clipboard.clone())
                .filter(|s| !s.is_empty())
        })
    }

    fn clip_set(&mut self, text: &str, linewise: bool) {
        let failed = crate::clipboard::set_text(text).is_err();
        crate::clipboard::remember_line_copy(text, linewise);
        if let Some(ed) = self.app.editor_mut() {
            ed.clipboard = text.to_string();
            ed.clipboard_is_line = linewise;
        }
        self.app.push_clipboard_history(text);
        if failed {
            self.app
                .notify("System clipboard unavailable — copied to internal clipboard only");
        }
    }

    fn act(&mut self, act: Act) -> Result<(), String> {
        act_on(self.app, act)
    }

    fn file_name(&self) -> Option<String> {
        let ed = self.app.editor()?;
        let path = ed.file_path.as_ref()?;
        Some(
            path.strip_prefix(vim_cwd(self.app))
                .unwrap_or(path)
                .display()
                .to_string(),
        )
    }

    fn cwd(&self) -> Option<std::path::PathBuf> {
        Some(vim_cwd(self.app).to_path_buf())
    }

    fn completion_visible(&self) -> bool {
        completion_visible(self.app)
    }

    fn buffers(&self) -> Vec<(usize, String, bool)> {
        self.app
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                (
                    i + 1,
                    tab.editor.display_name().to_string(),
                    tab.editor.modified,
                )
            })
            .collect()
    }

    fn commands(&self) -> Vec<String> {
        palette_ids_of(self.app)
    }
}

/// Run the engine with the workbench as host.
pub(crate) fn with_vim<R>(
    app: &mut App,
    f: impl FnOnce(&mut VimState, &mut AppHost<'_>) -> R,
) -> R {
    let mut vim = std::mem::take(&mut app.vim);
    let r = {
        let mut host = AppHost { app };
        f(&mut vim, &mut host)
    };
    // Preserve changes the workbench made while the engine ran.
    let enabled = vim.enabled;
    app.vim = vim;
    app.vim.enabled = enabled;
    r
}

/// Keyboard entry point, called before workbench shortcuts. Returns
/// `Some(quit)` when Vim consumed the key.
pub(crate) fn handle_key(app: &mut App, event: KeyEvent) -> Option<bool> {
    if !app.vim.enabled || app.vim_bypass {
        return None;
    }
    let key = Key::from_event(event)?;
    clear_yank_flash(app);
    if app.vim_terminal_prefix {
        app.vim_terminal_prefix = false;
        if key.is_ctrl('n') || key == Key::ESC {
            focus_editor(app);
            return Some(false);
        }
        if let Some(w) = window_key(key) {
            let _ = window(app, w, 1);
            return Some(false);
        }
        // Not a prefix command: send Ctrl-\ and the key to the terminal.
        app.vim_bypass = true;
        let quit = crate::input_keyboard::handle_keyboard_event_inner(
            app,
            KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL),
        );
        let quit2 = crate::input_keyboard::handle_keyboard_event_inner(app, event);
        app.vim_bypass = false;
        return Some(quit || quit2);
    }
    if app.focused_terminal_index().is_some() && app.focused_text_input_id().is_none() {
        if key.is_ctrl('\\') {
            app.vim_terminal_prefix = true;
            return Some(false);
        }
        return None;
    }
    if app.chat.visible && app.active_pane == ActivePane::Chat && !popup_open(app) {
        if key.is_ctrl('\\') {
            app.vim_terminal_prefix = true;
            return Some(false);
        }
        if key.is_ctrl('w') && app.vim.opts.ctrl_keys {
            app.vim_window_prefix = true;
            return Some(false);
        }
    }
    // A command line opened from a pane with no editor behind it (no file
    // open) takes every key until it closes, like the one in the editor.
    if app.vim.cmdline.is_some() && !owns_editor(app) && !popup_open(app) {
        return Some(feed_without_editor(app, key));
    }
    if !owns_editor(app) {
        // A one-line message is about the previous key, as in the editor.
        if app.vim.message.as_ref().is_some_and(|m| m.lines.is_empty()) {
            app.vim.message = None;
        }
        return other_pane_key(app, key, event);
    }
    sync_focus(app);
    with_vim(app, |vim, host| {
        vim.feed(host, key);
        after_key(vim, host);
    });
    Some(std::mem::take(&mut app.vim_quit))
}

/// `:` in an empty search box that has the keyboard by default (Settings,
/// the Extensions filter) opens the command line: a search can't start with
/// one, and these boxes are focused whenever the view is.
fn colon_over_empty_field(app: &App, key: Key) -> bool {
    use crate::text_input::TextInputId;
    key.char() == Some(':')
        && match app.focused_text_input_id() {
            Some(TextInputId::SettingsSearch) => app.settings_editor_search.is_empty(),
            Some(TextInputId::ExtensionsFilter) => app.extensions_filter.is_empty(),
            _ => false,
        }
}

/// Keys Vim takes before views that read the keyboard ahead of the Vim
/// layer (Commit Details owns every character): an open command line gets
/// its keys, and `:` opens one, unless the view is taking text.
pub(crate) fn early_key(app: &mut App, event: KeyEvent) -> Option<bool> {
    if !app.vim.enabled || app.vim_bypass || popup_open(app) {
        return None;
    }
    if !crate::ui_commit_details::focused(app) {
        return None;
    }
    let key = Key::from_event(event)?;
    if app.vim.cmdline.is_some() || app.vim_window_prefix || app.vim_terminal_prefix {
        return handle_key(app, event);
    }
    use crate::commit_details::Focus;
    let typing = matches!(app.commit_details.focus, Focus::Filter | Focus::Action(14));
    if typing {
        return None;
    }
    if key.char() == Some(':') || key.is_ctrl('w') && app.vim.opts.ctrl_keys {
        return handle_key(app, event);
    }
    commit_details_key(app, key)
}

/// Vim movement in Commit Details: j/k (and Ctrl-N/P) move through the
/// focused list, gg/G jump to its ends, Ctrl-D/U page.
fn commit_details_key(app: &mut App, key: Key) -> Option<bool> {
    let pending = std::mem::take(&mut app.vim_explorer_pending);
    if key.is_ctrl('d') {
        return translate(app, KeyCode::PageDown);
    }
    if key.is_ctrl('u') {
        return translate(app, KeyCode::PageUp);
    }
    if key.is_ctrl('n') {
        return translate(app, KeyCode::Down);
    }
    if key.is_ctrl('p') {
        return translate(app, KeyCode::Up);
    }
    match key.char()? {
        'g' if pending == Some('g') => translate(app, KeyCode::Home),
        'g' => {
            app.vim_explorer_pending = Some('g');
            Some(false)
        }
        'G' => translate(app, KeyCode::End),
        'j' => translate(app, KeyCode::Down),
        'k' => translate(app, KeyCode::Up),
        _ => None,
    }
}

/// Feed a command-line key to the engine while no editor owns the keyboard.
fn feed_without_editor(app: &mut App, key: Key) -> bool {
    with_vim(app, |vim, host| {
        vim.feed(host, key);
        after_key(vim, host);
    });
    std::mem::take(&mut app.vim_quit)
}

/// Leave Insert/Visual mode when another editor got focus.
pub(crate) fn sync_focus(app: &mut App) {
    let id = app.editor().map(|ed| ed.navigation_id).unwrap_or(0);
    if app.vim.last_nav_id != id {
        let old = app.vim.last_nav_id;
        with_vim(app, |vim, host| {
            if old != 0 && (vim.is_insert() || vim.is_visual() || vim.mode == Mode::Cmdline) {
                // The session belonged to another view.
                vim.insert = None;
                vim.cmdline = None;
                vim.mode = Mode::Normal;
                vim.pending.clear();
            }
            if let Some(ed) = host.ed() {
                if vim.mode == Mode::Normal {
                    ed.vim_selection = None;
                    ed.vim_block = None;
                    exec::after_native(vim, host);
                }
            }
            vim.last_nav_id = id;
        });
    }
}

fn after_key(vim: &mut VimState, host: &mut AppHost<'_>) {
    if let Some(ed) = host.app.editor() {
        vim.last_nav_id = ed.navigation_id;
    }
    if vim.mode == Mode::Normal {
        if let Some(ed) = host.ed() {
            if ed.vim_selection.is_some() && !vim.is_visual() {
                ed.vim_selection = None;
                ed.vim_block = None;
            }
            exec::clamp_normal(ed);
        }
    }
    if let Some((_, _, range, line)) = vim.yank_flash.take() {
        host.app.vim_yank_flash = Some((std::time::Instant::now(), range, line));
    }
}

/// After a mouse event in the editor.
pub(crate) fn after_mouse(app: &mut App) {
    if !app.vim.enabled || !vim_tab(app) || app.active_pane != ActivePane::Editor {
        return;
    }
    sync_focus(app);
    with_vim(app, |vim, host| {
        let Some(ed) = host.ed() else {
            return;
        };
        let has_sel = ed
            .selection_anchor
            .is_some_and(|a| a != (ed.cursor_line, ed.cursor_col))
            && ed.vim_selection.is_none();
        // The cell under the pointer: Vim's Visual mode includes it, unlike
        // a keyboard selection whose end is exclusive.
        let pointer = (ed.cursor_line, ed.cursor_col);
        let on_pointer = |vim: &mut VimState, ed: &mut Editor| {
            let max = Text::of(&ed.rope, ed.vim_buf.trailing_empty).max_normal_col(pointer.0);
            ed.cursor_line = pointer.0;
            ed.cursor_col = pointer.1.min(max);
            exec::sync_visual(vim, ed);
        };
        match vim.mode {
            Mode::Normal if has_sel => {
                exec::after_native(vim, host);
                if vim.is_visual()
                    && let Some(ed) = host.ed()
                {
                    on_pointer(vim, ed);
                }
            }
            Mode::Normal => exec::clamp_normal(ed),
            Mode::Visual(_) => {
                // A click without a drag ends Visual mode; a drag updates it.
                if ed.vim_selection.is_some() && ed.selection_anchor.is_none() {
                    exec::exit_visual(vim, host);
                } else if let Some(anchor) = ed.selection_anchor {
                    ed.vim_selection = None;
                    vim.visual_anchor = Pos::new(anchor.0, anchor.1);
                    on_pointer(vim, ed);
                } else {
                    exec::exit_visual(vim, host);
                }
            }
            Mode::Insert | Mode::Replace => {
                // Clicking elsewhere starts a new insert (like <Left>).
                let mode = vim.mode;
                if let Some(s) = &vim.insert {
                    let _ = s;
                }
                super::insert::finish(vim, host, false);
                super::insert::begin(vim, host, super::parse::InsertKind::Before, 1, false, None);
                vim.mode = mode;
            }
            Mode::Cmdline => {}
        }
        if let Some(ed) = host.ed() {
            exec::update_curswant(vim, ed);
        }
    });
}

/// Mapping timeouts; returns true when something changed.
pub(crate) fn tick(app: &mut App) -> bool {
    let mut changed = super::feedback::tick(app);
    if let Some((at, _, _)) = app.vim_yank_flash
        && at.elapsed() >= std::time::Duration::from_millis(180)
    {
        clear_yank_flash(app);
        changed = true;
    }
    if !app.vim.enabled || app.vim.next_deadline().is_none() {
        return changed;
    }
    let now = std::time::Instant::now();
    with_vim(app, |vim, host| vim.tick(host, now)) || changed
}

fn clear_yank_flash(app: &mut App) {
    if app.vim_yank_flash.take().is_some() {
        let normal = app.vim.mode == Mode::Normal;
        if let Some(ed) = app.editor_mut() {
            if normal {
                ed.vim_selection = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Other panes

fn window_key(key: Key) -> Option<Window> {
    let c = match key.k {
        K::Char(c) => c,
        K::Left => 'h',
        K::Right => 'l',
        K::Up => 'k',
        K::Down => 'j',
        _ => return None,
    };
    super::parse::window_cmd(c)
}

fn translate(app: &mut App, code: KeyCode) -> Option<bool> {
    app.vim_bypass = true;
    let quit = crate::input_keyboard::handle_keyboard_event_inner(
        app,
        KeyEvent::new(code, KeyModifiers::NONE),
    );
    app.vim_bypass = false;
    Some(quit)
}

/// Menus read their keys before the Vim layer, so translate Vim movement
/// for them up front: j/k (and Ctrl-N/P/J/K) move, l opens (h/l switch menus
/// in the menu bar), q closes. They have no letter accelerators to clash with.
pub(crate) fn menu_key(app: &App, event: KeyEvent) -> KeyEvent {
    if !app.vim.enabled || event.kind == crossterm::event::KeyEventKind::Release {
        return event;
    }
    let menu_bar = app.menu_active;
    let menu = menu_bar
        || app.tab_context_active
        || app.sc_context_active
        || app.text_input_context_menu.is_some()
        || crate::ui_status::gptcomplete_menu_open(app);
    if !menu {
        return event;
    }
    let Some(key) = Key::from_event(event) else {
        return event;
    };
    let code = if key.is_ctrl('n') || key.is_ctrl('j') {
        KeyCode::Down
    } else if key.is_ctrl('p') || key.is_ctrl('k') {
        KeyCode::Up
    } else {
        match key.char() {
            Some('j') => KeyCode::Down,
            Some('k') => KeyCode::Up,
            Some('h') if menu_bar => KeyCode::Left,
            Some('l') if menu_bar => KeyCode::Right,
            Some('l') => KeyCode::Enter,
            Some('q') => KeyCode::Esc,
            _ => return event,
        }
    };
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, event.kind)
}

/// Vim-style keys for sidebars, lists and popups.
fn other_pane_key(app: &mut App, key: Key, event: KeyEvent) -> Option<bool> {
    // Ctrl-W prefix works from every non-text pane.
    if app.vim_window_prefix {
        app.vim_window_prefix = false;
        match window_key(Key { ctrl: false, ..key }) {
            Some(w) => {
                if let Err(e) = window(app, w, 1) {
                    app.vim.set_error(e);
                }
            }
            None if key == Key::ESC => {}
            None => {
                app.vim.bell = Some(std::time::Instant::now());
                app.vim.set_message(format!(
                    "^W{}: not a window command (^W then h j k l, s, v, c, o...)",
                    super::keys::showcmd(&[key]).trim_start_matches("^W")
                ));
            }
        }
        return Some(false);
    }
    // Settings: its search box has focus, but Ctrl-W still moves between
    // windows (and Ctrl-W c closes the tab), as everywhere in Vim mode.
    if key.is_ctrl('w')
        && app.vim.opts.ctrl_keys
        && app.settings_editor_has_focus()
        && !app.settings_value_editing
    {
        app.vim_window_prefix = true;
        return Some(false);
    }
    if app.focused_text_input_id().is_some() && !colon_over_empty_field(app, key) {
        // The Extensions filter: Esc leaves it for the list, like leaving Insert mode.
        if key == Key::ESC
            && app.active_pane == ActivePane::Explorer
            && app.sidebar_view == crate::app::SidebarView::Extensions
            && app.ext_sidebar.search_focused
        {
            app.ext_sidebar.search_focused = false;
            return Some(false);
        }
        // Ctrl-U and Ctrl-W delete back to the start / over a word, as on
        // Vim's command line, before workbench shortcuts (Ctrl+W closes a tab)
        // can take them. The field's own handler then sees ordinary keys.
        if app.vim.opts.ctrl_keys
            && (key.is_ctrl('u') || key.is_ctrl('w'))
            && app.active_pane != ActivePane::Chat
        {
            let select = if key.is_ctrl('u') {
                (KeyCode::Home, KeyModifiers::SHIFT)
            } else {
                (KeyCode::Left, KeyModifiers::SHIFT | KeyModifiers::CONTROL)
            };
            app.vim_bypass = true;
            let a = crate::input_keyboard::handle_keyboard_event_inner(
                app,
                KeyEvent::new(select.0, select.1),
            );
            let b = crate::input_keyboard::handle_keyboard_event_inner(
                app,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            );
            app.vim_bypass = false;
            return Some(a || b);
        }
        // Text fields: Ctrl-N/P move in their result lists.
        if app.vim.opts.ctrl_keys
            && (app.palette.active
                || app.quick_open_active
                || app.symbol_list_active
                || app.goto_line_active)
        {
            if key.is_ctrl('n') || key.is_ctrl('j') {
                return translate(app, KeyCode::Down);
            }
            if key.is_ctrl('p') || key.is_ctrl('k') {
                return translate(app, KeyCode::Up);
            }
        }
        return None;
    }
    let list_popup = (app.lsp_code_action_active && !app.lsp_manager.code_actions.is_empty())
        || app.call_hierarchy_active
        || app.type_hierarchy_active
        || (app.lsp_references_active && !app.lsp_manager.reference_locations.is_empty())
        || app.explorer_context_active
        || app.context_menu_active
        || app.hunk_popup_active
        || app.sc_context_active
        || app.tab_context_active;
    if list_popup || app.palette.active || app.quick_open_active || app.symbol_list_active {
        if key.is_ctrl('n') || key.is_ctrl('j') {
            return translate(app, KeyCode::Down);
        }
        if key.is_ctrl('p') || key.is_ctrl('k') {
            return translate(app, KeyCode::Up);
        }
        if list_popup {
            match key.char() {
                Some('j') => return translate(app, KeyCode::Down),
                Some('k') => return translate(app, KeyCode::Up),
                Some('q') => return translate(app, KeyCode::Esc),
                Some('l') => return translate(app, KeyCode::Enter),
                _ => {}
            }
        }
        return None;
    }
    if popup_open(app) {
        return None;
    }
    // `:` opens the command line from any pane, as in Vim, where Ex commands
    // work from every window. They act on the editor, which takes focus.
    if key.char() == Some(':') && !(app.editor().is_some() && !app.tabs.is_empty()) {
        // No file is open: the command line opens over the pane that has
        // the keyboard, which keeps it, so `:Explore` or `:VT ...` still work.
        return Some(feed_without_editor(app, key));
    }
    if key.char() == Some(':') {
        if app.active_pane != ActivePane::Editor || vim_tab(app) {
            focus_editor(app);
        }
        if owns_editor(app) {
            sync_focus(app);
            with_vim(app, |vim, host| {
                vim.feed(host, key);
                after_key(vim, host);
            });
            return Some(std::mem::take(&mut app.vim_quit));
        }
        // A view with no Vim buffer (commit details, Settings, previews, a
        // diff): the command line opens over it, so `:q`, `:VT ...`, `:set`
        // and buffer commands still work from there.
        return Some(feed_without_editor(app, key));
    }
    if crate::ui_logs::focused(app) {
        // The Output panel: scroll and leave from the keyboard.
        if key.is_ctrl('w') && app.vim.opts.ctrl_keys {
            app.vim_window_prefix = true;
            return Some(false);
        }
        if key.is_ctrl('d') {
            for _ in 0..10 {
                translate(app, KeyCode::Down);
            }
            return Some(false);
        }
        if key.is_ctrl('u') {
            for _ in 0..10 {
                translate(app, KeyCode::Up);
            }
            return Some(false);
        }
        return match key.char() {
            Some('j') => translate(app, KeyCode::Down),
            Some('k') => translate(app, KeyCode::Up),
            Some('G') => translate(app, KeyCode::End),
            Some('q') => {
                focus_editor(app);
                Some(false)
            }
            _ if key == Key::ESC => {
                focus_editor(app);
                Some(false)
            }
            _ => None,
        };
    }
    if app.active_pane == ActivePane::Panel && app.show_panel && matches!(app.panel_tab, 1 | 3) {
        return panel_list_key(app, key);
    }
    if (app.active_pane == ActivePane::Explorer && app.show_explorer)
        || app.source_control_has_focus()
    {
        return explorer_key(app, key, event);
    }
    if app.active_pane == ActivePane::Editor && !vim_tab(app) {
        // Settings, previews and other tabs: Ctrl-W still navigates.
        if key.is_ctrl('w') && app.vim.opts.ctrl_keys {
            app.vim_window_prefix = true;
            return Some(false);
        }
        if app
            .editor()
            .is_some_and(|ed| ed.markdown_preview_scroll.is_some())
        {
            return match key.char() {
                Some('j') => translate(app, KeyCode::Down),
                Some('k') => translate(app, KeyCode::Up),
                Some('G') => translate(app, KeyCode::End),
                _ if key.is_ctrl('d') => translate(app, KeyCode::PageDown),
                _ if key.is_ctrl('u') => translate(app, KeyCode::PageUp),
                _ => None,
            };
        }
    }
    None
}

/// Problems and Notifications: j/k, gg/G, Ctrl-D/U, l or Enter opens, q/Esc
/// returns to the editor, Ctrl-W moves between windows.
fn panel_list_key(app: &mut App, key: Key) -> Option<bool> {
    if key.is_ctrl('w') && app.vim.opts.ctrl_keys {
        app.vim_window_prefix = true;
        return Some(false);
    }
    let pending = std::mem::take(&mut app.vim_explorer_pending);
    if key.is_ctrl('d') {
        return translate(app, KeyCode::PageDown);
    }
    if key.is_ctrl('u') {
        return translate(app, KeyCode::PageUp);
    }
    if key.is_ctrl('n') {
        return translate(app, KeyCode::Down);
    }
    if key.is_ctrl('p') {
        return translate(app, KeyCode::Up);
    }
    if key == Key::ESC {
        focus_editor(app);
        return Some(false);
    }
    match key.char()? {
        'g' if pending == Some('g') => translate(app, KeyCode::Home),
        'g' => {
            app.vim_explorer_pending = Some('g');
            Some(false)
        }
        'j' => translate(app, KeyCode::Down),
        'k' => translate(app, KeyCode::Up),
        'G' => translate(app, KeyCode::End),
        'l' | 'o' => translate(app, KeyCode::Enter),
        'q' => {
            focus_editor(app);
            Some(false)
        }
        c => {
            let typed = if pending == Some('g') {
                format!("g{c}")
            } else {
                c.to_string()
            };
            not_a_key(
                app,
                &typed,
                "j k gg G move, l opens, q returns, : runs a command",
            );
            Some(false)
        }
    }
}

/// A key that does nothing here: flash the badge and say what works.
fn not_a_key(app: &mut App, typed: &str, help: &str) {
    app.vim.bell = Some(std::time::Instant::now());
    app.vim
        .set_message(format!("{typed}: not a key here ({help})"));
}

fn explorer_key(app: &mut App, key: Key, _event: KeyEvent) -> Option<bool> {
    use crate::app::SidebarView;
    if key.is_ctrl('w') && app.vim.opts.ctrl_keys {
        app.vim_window_prefix = true;
        return Some(false);
    }
    let view = if app.source_control_has_focus() {
        SidebarView::SourceControl
    } else {
        app.sidebar_view
    };
    let pending = std::mem::take(&mut app.vim_explorer_pending);
    let Some(c) = key.char() else {
        if key.is_ctrl('d') {
            for _ in 0..10 {
                translate(app, KeyCode::Down);
            }
            return Some(false);
        }
        if key.is_ctrl('u') {
            for _ in 0..10 {
                translate(app, KeyCode::Up);
            }
            return Some(false);
        }
        if key.is_ctrl('n') {
            return translate(app, KeyCode::Down);
        }
        if key.is_ctrl('p') {
            return translate(app, KeyCode::Up);
        }
        if key == Key::ESC {
            focus_editor(app);
            return Some(false);
        }
        return None;
    };
    if pending == Some('g') {
        if c == 'g' && view == SidebarView::Explorer {
            app.explorer.selected = 0;
            app.set_explorer_single_selection(0);
            app.explorer.ensure_selection_visible();
        } else if c == 'g' {
            return translate(app, KeyCode::Home);
        } else {
            not_a_key(app, &format!("g{c}"), "gg goes to the top");
        }
        return Some(false);
    }
    match (view, c) {
        (_, 'j') => translate(app, KeyCode::Down),
        (_, 'k') => translate(app, KeyCode::Up),
        (SidebarView::Explorer, 'h') => translate(app, KeyCode::Left),
        (SidebarView::Explorer, 'l' | 'o') => translate(app, KeyCode::Enter),
        (SidebarView::Explorer, 'g') => {
            app.vim_explorer_pending = Some('g');
            Some(false)
        }
        (SidebarView::Explorer, 'G') => {
            let last = app.explorer.entries.len().saturating_sub(1);
            app.explorer.selected = last;
            app.set_explorer_single_selection(last);
            app.explorer.ensure_selection_visible();
            Some(false)
        }
        (SidebarView::Explorer, '/') => {
            app.execute_palette_command("explorer_find");
            Some(false)
        }
        (SidebarView::Explorer, 'a') => explorer_action(app, "New File"),
        (SidebarView::Explorer, 'A') => explorer_action(app, "New Folder"),
        (SidebarView::Explorer, 'r') => explorer_action(app, "Rename"),
        (SidebarView::Explorer, 'd') => explorer_action(app, "Delete"),
        (SidebarView::Explorer, 'y') => explorer_action(app, "Copy File"),
        (SidebarView::Explorer, 'x') => explorer_action(app, "Cut File"),
        (SidebarView::Explorer, 'p') => explorer_action(app, "Paste File"),
        (SidebarView::Explorer, 'Y') => explorer_action(app, "Copy Relative Path"),
        (SidebarView::Explorer, 'm') => {
            let (x, y) = app.explorer_area.map_or((0, 0), |a| (a.x + 2, a.y + 2));
            app.open_explorer_context_menu(x, y);
            Some(false)
        }
        (SidebarView::Explorer, 'R') => {
            app.execute_palette_command("git_refresh");
            Some(false)
        }
        // Search results and Source Control lists.
        (SidebarView::Search, 'h') => translate(app, KeyCode::Left),
        (
            SidebarView::Search
            | SidebarView::SourceControl
            | SidebarView::Extensions
            | SidebarView::Remote,
            'l' | 'o',
        ) => translate(app, KeyCode::Enter),
        (
            SidebarView::Search
            | SidebarView::SourceControl
            | SidebarView::Extensions
            | SidebarView::Remote,
            'g',
        ) => {
            app.vim_explorer_pending = Some('g');
            Some(false)
        }
        (
            SidebarView::Search
            | SidebarView::SourceControl
            | SidebarView::Extensions
            | SidebarView::Remote,
            'G',
        ) => translate(app, KeyCode::End),
        (_, 'q') => {
            focus_editor(app);
            Some(false)
        }
        _ => None,
    }
}

fn explorer_action(app: &mut App, label: &str) -> Option<bool> {
    let (x, y) = app.explorer_area.map_or((0, 0), |a| (a.x + 2, a.y + 2));
    app.open_explorer_context_menu(x, y);
    let idx = app
        .explorer_context_menu_items()
        .iter()
        .position(|item| *item == label);
    match idx {
        Some(idx) => {
            app.execute_explorer_context(idx);
        }
        None => {
            app.explorer_context_active = false;
        }
    }
    Some(false)
}

pub(crate) fn focus_editor(app: &mut App) {
    app.active_pane = ActivePane::Editor;
    app.terminal_panel_focused = false;
    if let Some(tab) = app.tabs.get(app.active_tab)
        && tab.kind == TabKind::Terminal
        && let Some(i) = app
            .tab_mru_order
            .iter()
            .copied()
            .find(|&i| app.tabs.get(i).is_some_and(|t| t.kind != TabKind::Terminal))
    {
        activate_tab(app, i);
    }
}

// ---------------------------------------------------------------------------
// Windows

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Group(usize),
    Explorer,
    Panel,
    TerminalSidebar,
    Chat,
}

fn rect_center(r: ratatui::layout::Rect) -> (i32, i32) {
    (
        r.x as i32 + r.width as i32 / 2,
        r.y as i32 + r.height as i32 / 2,
    )
}

fn visible_targets(app: &App) -> Vec<(Target, ratatui::layout::Rect)> {
    let mut out = Vec::new();
    // The Source Control tab draws without an editor area; its viewport is
    // the window Ctrl-W moves from.
    let editor_area = app
        .editor_area
        .or_else(|| (!app.tabs.is_empty()).then_some(app.viewport).flatten());
    if app.groups.is_empty() {
        if let Some(r) = editor_area {
            out.push((Target::Group(app.active_group), r));
        }
    } else {
        let mut leaves = Vec::new();
        if let Some(tree) = &app.split_tree {
            tree.visit_leaves(&mut |l| leaves.push(l));
        }
        if leaves.is_empty() {
            leaves = (0..app.groups.len()).collect();
        }
        for i in leaves {
            let area = if i == app.active_group {
                editor_area
            } else {
                app.groups
                    .get(i)
                    .filter(|g| g.desk == app.desks.active)
                    .and_then(|g| g.editor_area)
            };
            if let Some(r) = area {
                out.push((Target::Group(i), r));
            }
        }
    }
    if app.show_explorer
        && let Some(r) = app.explorer_area
    {
        out.push((Target::Explorer, r));
    }
    if app.show_panel
        && let Some(r) = app.panel_area
    {
        out.push((Target::Panel, r));
    }
    if app.show_terminal_sidebar
        && let Some(r) = app.terminal_sidebar_area
    {
        out.push((Target::TerminalSidebar, r));
    }
    if let Some(r) = chat_dock_area(app) {
        out.push((Target::Chat, r));
    }
    out
}

/// The docked Agent Chat, when it is on screen.
fn chat_dock_area(app: &App) -> Option<ratatui::layout::Rect> {
    if !app.chat.visible || !app.chat.layout.dock_visible {
        return None;
    }
    app.chat
        .layout
        .surfaces
        .iter()
        .find(|s| s.key.is_none())
        .map(|s| s.area)
        .filter(|r| !r.is_empty())
}

/// Keys go to the docked Agent Chat (not a conversation in an editor tab).
fn chat_dock_focused(app: &App) -> bool {
    app.chat.visible && app.active_pane == ActivePane::Chat && app.chat.layout.active.is_none()
}

fn current_target(app: &App) -> Target {
    if app.terminal_panel_focused {
        return Target::TerminalSidebar;
    }
    if chat_dock_focused(app) {
        return Target::Chat;
    }
    match app.active_pane {
        ActivePane::Explorer => Target::Explorer,
        ActivePane::Panel => Target::Panel,
        _ => Target::Group(app.active_group),
    }
}

fn focus_target(app: &mut App, t: Target) {
    let before = current_target(app);
    // Only Vim window commands move focus here (the engine may be taken out
    // of the App at this point, so its `enabled` flag cannot be read).
    if before != t
        && let Some((_, area)) = visible_targets(app)
            .into_iter()
            .find(|(target, _)| *target == t)
    {
        super::feedback::flash_focus(app, area);
    }
    if before != t {
        app.vim_last_window = Some(before_id(before));
    }
    if app.vim.enabled {
        // Leaving the editor ends Insert/Visual mode, as in Vim.
        with_vim(app, |vim, host| vim.reset_to_normal(host));
    }
    match t {
        Target::Group(i) => {
            if i != app.active_group {
                app.switch_group(i);
            }
            app.active_pane = ActivePane::Editor;
            app.terminal_panel_focused = false;
        }
        Target::Explorer => {
            // Keep the view that is showing (Source Control, Search...).
            if !app.sidebar_is_visible() {
                app.show_explorer_view();
            }
            app.active_pane = ActivePane::Explorer;
            app.terminal_panel_focused = false;
        }
        Target::Panel => {
            // The bottom panel holds Problems, Output and Notifications.
            app.active_pane = ActivePane::Panel;
            app.terminal_panel_focused = false;
            if app.panel_tab == 2 {
                app.show_output();
            }
        }
        Target::TerminalSidebar => {
            app.active_pane = ActivePane::Editor;
            app.terminal_panel_focused = true;
        }
        Target::Chat => {
            app.chat.layout.active = None;
            app.focus_chat();
        }
    }
}

fn before_id(t: Target) -> usize {
    match t {
        Target::Group(i) => i,
        Target::Explorer => usize::MAX - 1,
        Target::Panel => usize::MAX - 2,
        Target::TerminalSidebar => usize::MAX - 3,
        Target::Chat => usize::MAX - 4,
    }
}

fn from_id(id: usize) -> Target {
    match id {
        x if x == usize::MAX - 1 => Target::Explorer,
        x if x == usize::MAX - 2 => Target::Panel,
        x if x == usize::MAX - 3 => Target::TerminalSidebar,
        x if x == usize::MAX - 4 => Target::Chat,
        i => Target::Group(i),
    }
}

fn directional(app: &App, dx: i32, dy: i32) -> Option<Target> {
    let targets = visible_targets(app);
    let cur = current_target(app);
    let cur_rect = targets.iter().find(|(t, _)| *t == cur).map(|(_, r)| *r)?;
    let (cx, cy) = rect_center(cur_rect);
    let mut best: Option<(i64, Target)> = None;
    for (t, r) in &targets {
        if *t == cur {
            continue;
        }
        // Must lie in the requested direction, beyond the current edge.
        let ok = match (dx, dy) {
            (-1, _) => r.right() <= cur_rect.x + 1,
            (1, _) => r.x + 1 >= cur_rect.right(),
            (_, -1) => r.bottom() <= cur_rect.y + 1,
            _ => r.y + 1 >= cur_rect.bottom(),
        };
        if !ok {
            continue;
        }
        // Prefer overlap on the perpendicular axis, then distance.
        let overlap = if dx != 0 {
            (r.bottom().min(cur_rect.bottom()) as i64 - r.y.max(cur_rect.y) as i64).max(0)
        } else {
            (r.right().min(cur_rect.right()) as i64 - r.x.max(cur_rect.x) as i64).max(0)
        };
        let (tx, ty) = rect_center(*r);
        let dist = ((tx - cx).abs() + (ty - cy).abs()) as i64;
        let score = dist - overlap * 4;
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, *t));
        }
    }
    best.map(|(_, t)| t)
}

pub(crate) fn window(app: &mut App, w: Window, count: usize) -> Result<(), String> {
    let groups: Vec<usize> = visible_targets(app)
        .into_iter()
        .filter_map(|(t, _)| match t {
            Target::Group(i) => Some(i),
            _ => None,
        })
        .collect();
    match w {
        Window::Left | Window::Right | Window::Up | Window::Down => {
            let (dx, dy) = match w {
                Window::Left => (-1, 0),
                Window::Right => (1, 0),
                Window::Up => (0, -1),
                _ => (0, 1),
            };
            for _ in 0..count.max(1) {
                match directional(app, dx, dy) {
                    Some(t) => focus_target(app, t),
                    None => break,
                }
            }
            Ok(())
        }
        Window::Next | Window::Prev | Window::Top | Window::Bottom => {
            if groups.len() < 2 {
                // Cycle between the editor and the visible side panes.
                let all: Vec<Target> = visible_targets(app).into_iter().map(|(t, _)| t).collect();
                let cur = current_target(app);
                let i = all.iter().position(|t| *t == cur).unwrap_or(0);
                if all.len() > 1 {
                    let n = all.len();
                    let next = if w == Window::Prev {
                        all[(i + n - 1) % n]
                    } else {
                        all[(i + 1) % n]
                    };
                    focus_target(app, next);
                }
                return Ok(());
            }
            let cur = groups
                .iter()
                .position(|&g| g == app.active_group)
                .unwrap_or(0);
            let n = groups.len();
            let next = match w {
                Window::Top => 0,
                Window::Bottom => n - 1,
                Window::Prev => (cur + n - 1) % n,
                _ => (cur + 1) % n,
            };
            focus_target(app, Target::Group(groups[next]));
            Ok(())
        }
        Window::Last => {
            let Some(id) = app.vim_last_window else {
                return Err("E441: There is no previous window".into());
            };
            focus_target(app, from_id(id));
            Ok(())
        }
        Window::Split | Window::VSplit | Window::New => {
            if app.tabs.is_empty() {
                return Ok(());
            }
            app.execute_palette_command("duplicate_tab");
            let direction = if w == Window::VSplit {
                crate::app::SplitDirection::Horizontal
            } else {
                crate::app::SplitDirection::Vertical
            };
            app.create_split_group_dir(app.active_tab, direction);
            if w == Window::New {
                app.new_file();
            }
            app.active_pane = ActivePane::Editor;
            Ok(())
        }
        Window::Close if chat_dock_focused(app) => {
            app.chat_action(crate::app_acp::ChatAction::Hide);
            focus_editor(app);
            Ok(())
        }
        Window::Close if !vim_tab(app) && !app.tabs.is_empty() => {
            // Settings, previews and other non-text tabs close like a window.
            app.close_tab(app.active_tab);
            Ok(())
        }
        Window::Close => {
            if groups.len() < 2 {
                return Err("E444: Cannot close last window".into());
            }
            let modified = app
                .tabs
                .get(app.active_tab)
                .is_some_and(|t| t.has_unsaved_changes());
            let shared = app.editor().map(|e| e.save_identity).is_some_and(|id| {
                app.groups.iter().enumerate().any(|(i, g)| {
                    i != app.active_group && g.tabs.iter().any(|t| t.editor.save_identity == id)
                })
            });
            if modified && !shared {
                return Err("E37: No write since last change (add ! to override)".into());
            }
            app.force_close_tab(app.active_tab);
            Ok(())
        }
        Window::Only => {
            app.execute_palette_command("close_split");
            Ok(())
        }
        Window::MaxHeight | Window::MaxWidth => {
            app.execute_palette_command("toggle_maximized");
            Ok(())
        }
        Window::Equalize => {
            app.execute_palette_command("vim_equalize");
            Ok(())
        }
        Window::Rotate
        | Window::Exchange
        | Window::MoveLeft
        | Window::MoveRight
        | Window::MoveUp
        | Window::MoveDown
        | Window::ToTab => {
            Err("E443: Moving windows is not supported; drag the tab instead".into())
        }
    }
}

// ---------------------------------------------------------------------------
// Tabs and buffers

pub(crate) fn activate_tab(app: &mut App, idx: usize) {
    if idx >= app.tabs.len() || idx == app.active_tab {
        return;
    }
    app.sync_duplicate_file_tabs();
    app.clear_transient_editor_state();
    app.auto_save_on_focus_change();
    app.active_tab = idx;
    app.validate_diff_state_for_active_tab();
    app.touch_tab_mru(app.active_tab);
    app.refresh_git_line_status_async();
    app.active_pane = ActivePane::Editor;
    app.terminal_panel_focused = false;
    app.ensure_active_tab_visible();
}

fn any_modified(app: &App) -> bool {
    app.tabs
        .iter()
        .chain(app.groups.iter().flat_map(|g| &g.tabs))
        .any(|t| t.has_unsaved_changes())
}

fn quit_app(app: &mut App, choice: char) -> Result<(), String> {
    app.exit_confirm_active = true;
    let key = KeyEvent::new(KeyCode::Char(choice), KeyModifiers::NONE);
    match crate::input_dialogs::handle_exit_confirm_key(app, key) {
        Some(true) => {
            app.vim_quit = true;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The directory relative Ex paths start from: the `:cd` directory, else the
/// workspace folder.
pub(crate) fn vim_cwd(app: &App) -> &std::path::Path {
    app.vim_cwd
        .as_ref()
        .filter(|(root, _)| *root == app.explorer.root)
        .map_or(&app.explorer.root, |(_, dir)| dir)
}

/// `:cd`, `:lcd`, `:tcd`: change the directory relative Ex paths, `:!` and
/// completion use. The workspace folder stays open, as Vim's `:cd` leaves
/// the buffers alone. No argument goes home (on Windows it prints the
/// directory, as Windows Vim does); `-` returns to the previous one.
fn change_dir(app: &mut App, arg: Option<String>) -> Result<(), String> {
    let current = vim_cwd(app).to_path_buf();
    let target = match arg.as_deref().map(str::trim) {
        None | Some("") if cfg!(windows) => return info(current.display().to_string()),
        None | Some("") => std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| "E472: HOME is not set".to_string())?,
        Some("-") => app
            .vim_prev_cwd
            .clone()
            .ok_or_else(|| "E186: No previous directory".to_string())?,
        Some(dir) => open_path(app, dir),
    };
    let target = target
        .canonicalize()
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| {
            format!(
                "E344: Can't find directory \"{}\" in cdpath",
                arg.unwrap_or_default().trim()
            )
        })?;
    let target = crate::portable::filesystem_spelling(&target);
    app.vim_prev_cwd = Some(current);
    app.vim_cwd = (target != app.explorer.root).then(|| (app.explorer.root.clone(), target));
    Ok(())
}

fn open_path(app: &mut App, path: &str) -> std::path::PathBuf {
    let p = path.trim();
    let expanded = if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| std::path::PathBuf::from(h).join(rest))
            .unwrap_or_else(|| p.into())
    } else {
        std::path::PathBuf::from(p)
    };
    if expanded.is_relative() {
        vim_cwd(app).join(expanded)
    } else {
        expanded
    }
}

fn find_view(app: &App, nav_id: u64) -> Option<(usize, usize)> {
    if let Some(i) = app
        .tabs
        .iter()
        .position(|t| t.editor.navigation_id == nav_id)
    {
        return Some((app.active_group, i));
    }
    app.groups.iter().enumerate().find_map(|(g, group)| {
        (g != app.active_group)
            .then(|| {
                group
                    .tabs
                    .iter()
                    .position(|t| t.editor.navigation_id == nav_id)
                    .map(|i| (g, i))
            })
            .flatten()
    })
}

pub(crate) fn act_on(app: &mut App, act: Act) -> Result<(), String> {
    match act {
        Act::Command(id) => command(app, &id),
        Act::Write { path, force } => {
            let Some(ed) = app.editor() else {
                return Err("E32: No file name".into());
            };
            if ed.read_only && !force && path.is_none() {
                return Err("E45: 'readonly' option is set (add ! to override)".into());
            }
            let current = ed.file_path.clone();
            let intended = ed.vim_buf.intended_path.clone();
            let path = path.or_else(|| {
                current
                    .is_none()
                    .then(|| intended.map(|p| p.display().to_string()))
                    .flatten()
            });
            match path {
                Some(p) => {
                    let target = open_path(app, &p);
                    if current.as_ref() == Some(&target) {
                        app.save_current_file();
                        return Ok(());
                    }
                    if target.exists() && !force {
                        return Err(format!("E13: File exists (add ! to override): {p}"));
                    }
                    app.execute_palette_command("save_as");
                    app.save_as_query = target.display().to_string();
                    app.execute_save_as();
                    if let Some(err) = app.modal.error.take() {
                        app.save_as_active = false;
                        app.modal = Default::default();
                        return Err(err);
                    }
                    app.save_as_active = false;
                    app.modal = Default::default();
                    Ok(())
                }
                None => {
                    if current.is_none() {
                        app.execute_palette_command("save_as");
                        return Ok(());
                    }
                    app.save_current_file();
                    Ok(())
                }
            }
        }
        Act::WriteAll => {
            app.execute_palette_command("save_all");
            Ok(())
        }
        Act::Quit { force } => {
            let last = app.tabs.len() <= 1
                && app
                    .groups
                    .iter()
                    .enumerate()
                    .all(|(i, g)| i == app.active_group || g.tabs.is_empty());
            let modified = app
                .tabs
                .get(app.active_tab)
                .is_some_and(|t| t.has_unsaved_changes());
            let shared = app.editor().map(|e| e.save_identity).is_some_and(|id| {
                let me = app.editor().map(|e| e.navigation_id);
                app.tabs
                    .iter()
                    .chain(app.groups.iter().flat_map(|g| &g.tabs))
                    .any(|t| Some(t.editor.navigation_id) != me && t.editor.save_identity == id)
            });
            if modified && !force && !shared {
                return Err("E37: No write since last change (add ! to override)".into());
            }
            if last {
                if any_modified(app) && !force {
                    return Err("E37: No write since last change (add ! to override)".into());
                }
                return quit_app(app, if force { 'd' } else { 'y' });
            }
            if force {
                app.force_close_tab(app.active_tab);
            } else {
                app.close_tab(app.active_tab);
            }
            Ok(())
        }
        Act::QuitAll { force } => {
            if !force && any_modified(app) {
                let name = app
                    .tabs
                    .iter()
                    .chain(app.groups.iter().flat_map(|g| &g.tabs))
                    .find(|t| t.has_unsaved_changes())
                    .map(|t| t.editor.display_name().to_string())
                    .unwrap_or_default();
                return Err(format!(
                    "E37: No write since last change for buffer \"{name}\" (add ! to override)"
                ));
            }
            quit_app(app, if force { 'd' } else { 'y' })
        }
        Act::WriteQuitAll => quit_app(app, 'y'),
        Act::WriteQuit { force } => {
            let Some(ed) = app.editor() else {
                return Err("E32: No file name".into());
            };
            if ed.read_only && !force {
                return Err("E45: 'readonly' option is set (add ! to override)".into());
            }
            if ed.file_path.is_none() && ed.vim_buf.intended_path.is_none() {
                app.execute_palette_command("save_as");
                return Err("E32: No file name".into());
            }
            if ed.file_path.is_none() {
                act_on(app, Act::Write { path: None, force })?;
                return Ok(());
            }
            let last = app.tabs.len() <= 1
                && app
                    .groups
                    .iter()
                    .enumerate()
                    .all(|(i, g)| i == app.active_group || g.tabs.is_empty());
            if last {
                // The workbench's own save-everything-and-exit path.
                return quit_app(app, 'y');
            }
            let idx = app.active_tab;
            let (trim, newline) = (app.trim_trailing_on_save, app.insert_final_newline);
            if let Err(error) = app.tabs[idx].editor.save_file_with_options(trim, newline) {
                return Err(format!("E212: Can't open file for writing: {error}"));
            }
            app.force_close_tab(idx);
            Ok(())
        }
        Act::Edit { path, force } => match path {
            None => {
                let modified = app.editor().is_some_and(|e| e.modified);
                if modified && !force {
                    return Err("E37: No write since last change (add ! to override)".into());
                }
                app.execute_palette_command("revert_file");
                Ok(())
            }
            Some(p) => {
                let path = open_path(app, &p);
                if path.is_dir() {
                    app.show_explorer_view();
                    app.active_pane = ActivePane::Explorer;
                    return Ok(());
                }
                if !path.exists() {
                    app.new_file();
                    if let Some(ed) = app.editor_mut() {
                        ed.suggested_file_name =
                            path.file_name().map(|n| n.to_string_lossy().to_string());
                        ed.vim_buf.intended_path = Some(path);
                    }
                    return Ok(());
                }
                app.push_nav_history();
                app.open_file(&path);
                app.active_pane = ActivePane::Editor;
                Ok(())
            }
        },
        Act::NewBuffer => {
            app.new_file();
            Ok(())
        }
        Act::Window(w, count) => window(app, w, count),
        Act::TabNext(n) => {
            if app.tabs.is_empty() {
                return Ok(());
            }
            let idx = match n {
                Some(usize::MAX) => app.tabs.len() - 1,
                Some(n) => n.saturating_sub(1).min(app.tabs.len() - 1),
                None => (app.active_tab + 1) % app.tabs.len(),
            };
            activate_tab(app, idx);
            Ok(())
        }
        Act::TabPrev(n) => {
            if app.tabs.is_empty() {
                return Ok(());
            }
            let len = app.tabs.len();
            let idx = (app.active_tab + len - (n.max(1) % len)) % len;
            activate_tab(app, idx);
            Ok(())
        }
        Act::BufferNext(n) => {
            if app.tabs.is_empty() {
                return Ok(());
            }
            activate_tab(app, (app.active_tab + n) % app.tabs.len());
            Ok(())
        }
        Act::BufferPrev(n) => act_on(app, Act::TabPrev(n)),
        Act::Buffer(name) => {
            if app.tabs.is_empty() {
                return Ok(());
            }
            let idx = if name == "$" {
                Some(app.tabs.len() - 1)
            } else if let Ok(n) = name.parse::<usize>() {
                (n >= 1 && n <= app.tabs.len()).then(|| n - 1)
            } else {
                let lower = name.to_lowercase();
                let matches: Vec<usize> = app
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| {
                        t.editor.display_name().to_lowercase().contains(&lower)
                            || t.editor.file_path.as_ref().is_some_and(|p| {
                                p.display().to_string().to_lowercase().contains(&lower)
                            })
                    })
                    .map(|(i, _)| i)
                    .collect();
                match matches.len() {
                    0 => return Err(format!("E94: No matching buffer for {name}")),
                    1 => Some(matches[0]),
                    _ => matches
                        .iter()
                        .copied()
                        .find(|&i| app.tabs[i].editor.display_name().to_lowercase() == lower)
                        .or(Some(matches[0])),
                }
            };
            match idx {
                Some(i) => {
                    activate_tab(app, i);
                    Ok(())
                }
                None => Err(format!("E86: Buffer {name} does not exist")),
            }
        }
        Act::BufferDelete { force } | Act::TabClose { force } => {
            if !force
                && app
                    .tabs
                    .get(app.active_tab)
                    .is_some_and(|t| t.has_unsaved_changes())
            {
                return Err(
                    "E89: No write since last change for buffer (add ! to override)".into(),
                );
            }
            if force {
                app.force_close_tab(app.active_tab);
            } else {
                app.close_tab(app.active_tab);
            }
            Ok(())
        }
        Act::TabOnly => {
            app.execute_palette_command("close_others");
            Ok(())
        }
        Act::Alternate => {
            let prev = app
                .tab_mru_order
                .iter()
                .copied()
                .find(|&i| i != app.active_tab && i < app.tabs.len());
            match prev {
                Some(i) => {
                    activate_tab(app, i);
                    Ok(())
                }
                None => Err("E23: No alternate file".into()),
            }
        }
        Act::JumpTo { nav_id, path, pos } => {
            if let Some((g, i)) = find_view(app, nav_id) {
                if g != app.active_group {
                    app.switch_group(g);
                }
                activate_tab(app, i);
            } else if let Some(path) = path {
                app.open_file(&path);
            } else {
                return Err("E92: Buffer not found".into());
            }
            if let Some(ed) = app.editor_mut() {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let p = t.clamp_normal(pos);
                ed.set_cursor(p.line, p.col);
            }
            Ok(())
        }
        Act::Quickfix(cmd) => {
            match cmd {
                super::Quickfix::Open => {
                    crate::ui_panels::show_problems(app);
                    app.active_pane = ActivePane::Panel;
                    app.terminal_panel_focused = false;
                }
                super::Quickfix::Close => {
                    if app.show_panel && app.panel_tab == 1 {
                        if app.active_pane == ActivePane::Panel {
                            focus_editor(app);
                        }
                        crate::ui_panels::close_panel(app);
                    }
                }
                super::Quickfix::Next => {
                    app.execute_palette_command("next_diagnostic");
                }
                super::Quickfix::Prev => {
                    app.execute_palette_command("prev_diagnostic");
                }
            }
            Ok(())
        }
        Act::Explore => {
            app.show_explorer_view();
            app.active_pane = ActivePane::Explorer;
            app.terminal_panel_focused = false;
            Ok(())
        }
        Act::Terminal(_) => {
            app.execute_palette_command("new_terminal");
            Ok(())
        }
        Act::SetOption { name, value } => set_option(app, &name, &value),
        Act::Cd(dir) => change_dir(app, dir),
        Act::YankFlash {
            start,
            end,
            linewise,
        } => {
            let Some(ed) = app.editor_mut() else {
                return Ok(());
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let range = if linewise {
                let (el, ec) = if end.line + 1 < ed.rope.len_lines() {
                    (end.line + 1, 0)
                } else {
                    (end.line, t.len(end.line))
                };
                (start.line, 0, el, ec)
            } else {
                let e = (t.idx(end) + 1).min(ed.rope.len_chars());
                let ep = t.pos(e);
                let ep = if e == t.line_next_start(end.line) && end.line + 1 < ed.rope.len_lines() {
                    Pos::new(end.line + 1, 0)
                } else {
                    ep
                };
                (start.line, start.col, ep.line, ep.col)
            };
            if range.0 == range.2 && range.1 == range.3 {
                return Ok(());
            }
            ed.vim_selection = Some(range);
            app.vim_yank_flash = Some((std::time::Instant::now(), range, linewise));
            Ok(())
        }
    }
}

fn info(msg: String) -> Result<(), String> {
    Err(format!("\u{1}{msg}"))
}

fn set_option(app: &mut App, name: &str, value: &str) -> Result<(), String> {
    let query = value == "?";
    let flag = |cur: bool| -> bool {
        match value {
            "!" => !cur,
            "&" => false,
            "false" => false,
            _ => true,
        }
    };
    match name {
        "number" => {
            if query {
                return info(format!(
                    "{}number",
                    if app.show_line_numbers { "  " } else { "no" }
                ));
            }
            app.show_line_numbers = flag(app.show_line_numbers);
        }
        "relativenumber" => {
            if query {
                return info(format!(
                    "{}relativenumber",
                    if app.relative_line_numbers {
                        "  "
                    } else {
                        "no"
                    }
                ));
            }
            app.relative_line_numbers = flag(app.relative_line_numbers);
            if app.relative_line_numbers {
                app.show_line_numbers = true;
            }
        }
        "wrap" => {
            let cur = app.editor().is_some_and(|e| e.word_wrap);
            if query {
                return info(format!("{}wrap", if cur { "  " } else { "no" }));
            }
            if flag(cur) != cur {
                app.execute_palette_command("toggle_word_wrap");
            }
        }
        "list" => {
            let cur = app
                .editor()
                .is_some_and(|e| e.whitespace_mode != crate::app::WhitespaceMode::None);
            if query {
                return info(format!("{}list", if cur { "  " } else { "no" }));
            }
            if flag(cur) != cur {
                app.execute_palette_command("toggle_whitespace");
            }
        }
        "expandtab" => {
            let Some(ed) = app.editor_mut() else {
                return Ok(());
            };
            if query {
                return info(format!(
                    "{}expandtab",
                    if ed.use_tabs { "no" } else { "  " }
                ));
            }
            ed.use_tabs = !flag(!ed.use_tabs);
        }
        "readonly" => {
            let Some(ed) = app.editor_mut() else {
                return Ok(());
            };
            if query {
                return info(format!(
                    "{}readonly",
                    if ed.read_only { "  " } else { "no" }
                ));
            }
            let v = flag(ed.read_only);
            ed.set_read_only(v);
        }
        "modifiable" => {
            let Some(ed) = app.editor_mut() else {
                return Ok(());
            };
            if query {
                return info(format!(
                    "{}modifiable",
                    if ed.read_only { "no" } else { "  " }
                ));
            }
            let v = !flag(!ed.read_only);
            ed.set_read_only(v);
        }
        "tabstop" | "shiftwidth" | "softtabstop" => {
            let Some(ed) = app.editor_mut() else {
                return Ok(());
            };
            if query {
                return info(format!("  {name}={}", ed.tab_size));
            }
            let n: usize = value
                .parse()
                .map_err(|_| format!("E521: Number required after =: {name}={value}"))?;
            if n == 0 && name == "tabstop" {
                return Err("E487: Argument must be positive".into());
            }
            if n > 0 {
                ed.tab_size = n;
            }
        }
        "fileformat" => {
            if query {
                let ff = app.editor().map_or("unix", |e| {
                    if e.cached_line_ending == "CRLF" {
                        "dos"
                    } else {
                        "unix"
                    }
                });
                return info(format!("  fileformat={ff}"));
            }
            match value {
                "unix" => {
                    app.execute_palette_command("eol_lf");
                }
                "dos" => {
                    app.execute_palette_command("eol_crlf");
                }
                _ => return Err(format!("E474: Invalid argument: fileformat={value}")),
            }
        }
        "filetype" | "syntax" => {
            if query {
                return info(format!(
                    "  filetype={}",
                    app.editor()
                        .map_or("", |e| e.language_name())
                        .to_lowercase()
                ));
            }
            if let Some(ed) = app.editor_mut() {
                ed.language_override = Some(value.to_string());
            }
        }
        "scrolloff" => {
            let n: usize = value.parse().unwrap_or(0);
            app.default_cursor_surrounding_lines = n;
            for tab in app.tabs.iter_mut() {
                tab.editor.cursor_surrounding_lines = n;
            }
        }
        "colorscheme" => {
            if !app.set_theme_by_key(value) {
                return Err(format!("E185: Cannot find color scheme '{value}'"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Workbench commands, including Vim-specific ones.
fn command(app: &mut App, id: &str) -> Result<(), String> {
    match id {
        "vim_escape" => {
            // Esc in Normal mode also closes the transient UI around the editor.
            app.dismiss_vtcomplete();
            app.lsp_completion_active = false;
            app.lsp_signature_active = false;
            app.dismiss_inline();
            app.clear_lsp_hover();
            app.keyboard_hover = false;
            if app.search_active
                && !matches!(
                    app.focused_text_input_id(),
                    Some(
                        crate::text_input::TextInputId::SearchFind
                            | crate::text_input::TextInputId::SearchReplace
                    )
                )
            {
                app.search_active = false;
            }
            Ok(())
        }
        "vim_dismiss_popups" => {
            app.dismiss_vtcomplete();
            app.lsp_completion_active = false;
            app.lsp_signature_active = false;
            app.dismiss_inline();
            Ok(())
        }
        "vim_complete" => {
            app.update_vtcomplete_explicit();
            app.request_lsp_suggestions(false);
            Ok(())
        }
        "vim_hover" => {
            let Some(ed) = app.editor() else {
                return Ok(());
            };
            let Some(path) = ed.file_path.clone() else {
                return Err("E349: No identifier under cursor".into());
            };
            let (line, col) = (ed.cursor_line, ed.cursor_col);
            let col16 = crate::lsp_positions::to_utf16(&ed.rope, line, col);
            let version = ed.content_version;
            let rope = ed.rope.clone();
            app.queue_lsp_document_sync(path.clone(), version, rope);
            app.flush_lsp_document_syncs();
            app.lsp_hover_line = line;
            app.lsp_hover_col = col;
            app.lsp_hover_suppressed = false;
            app.keyboard_hover = true;
            app.lsp_hover_active = app.lsp_manager.request_hover(&path, line as u32, col16);
            if !app.lsp_hover_active {
                app.keyboard_hover = false;
                return Err("No hover information (no language server)".into());
            }
            Ok(())
        }
        "vim_open_url" => {
            let Some(ed) = app.editor() else {
                return Ok(());
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let line = t.line_string(ed.cursor_line);
            let col = ed.cursor_col;
            let url = line
                .split(|c: char| c.is_whitespace() || "<>\"'()[]{}`".contains(c))
                .scan(0usize, |pos, word| {
                    let start = *pos;
                    *pos += word.chars().count() + 1;
                    Some((start, word.to_string()))
                })
                .find(|(s, w)| {
                    col >= *s
                        && col <= s + w.chars().count()
                        && (w.starts_with("http://")
                            || w.starts_with("https://")
                            || w.starts_with("file://")
                            || w.starts_with("www."))
                })
                .map(|(_, w)| w);
            match url {
                Some(u) => {
                    app.open_external(crate::external_open::Request::Url(u));
                    Ok(())
                }
                None => {
                    app.execute_palette_command("open_file_at_cursor");
                    Ok(())
                }
            }
        }
        "vim_equalize" => Ok(()),
        _ => {
            let ids = palette_ids_of(app);
            if !ids.iter().any(|c| c == id) && !id.starts_with("vim_") {
                // Shortcut ids name a few commands differently (go_to_line
                // runs goto_line): accept those too.
                if let Some(alias) = ids
                    .iter()
                    .find(|c| crate::app_menu::keybinding_id_for_command(c) == id)
                {
                    let alias = alias.clone();
                    return command(app, &alias);
                }
                return Err(format!("E492: Unknown VTCode command: {id}"));
            }
            let sidebar = sidebar_state(app);
            app.execute_palette_command(id);
            // Ex commands run with the engine taken out of the App and end in
            // Normal mode, so only the focus moves here.
            if revealed_sidebar(app, sidebar) {
                app.active_pane = ActivePane::Explorer;
                app.terminal_panel_focused = false;
            }
            Ok(())
        }
    }
}

/// Whether a command just revealed a list view that should take the keyboard.
fn revealed_sidebar(app: &App, (view, visible): (crate::app::SidebarView, bool)) -> bool {
    use crate::app::SidebarView;
    app.sidebar_is_visible()
        && !(view == SidebarView::SourceControl
            && app.sidebar_view == SidebarView::Explorer
            && app.source_control_tab_active())
        && !(visible && view == app.sidebar_view)
        && matches!(
            app.sidebar_view,
            SidebarView::Explorer
                | SidebarView::SourceControl
                | SidebarView::Extensions
                | SidebarView::Remote
        )
        && app.active_pane != ActivePane::Explorer
}

/// The sidebar view and whether it is shown, before a command runs.
pub(crate) fn sidebar_state(app: &App) -> (crate::app::SidebarView, bool) {
    (app.sidebar_view, app.sidebar_is_visible())
}

/// A command that reveals the Explorer, Source Control, Extensions or Remote
/// view also moves the keyboard there, as a Vim user expects from a toggle
/// such as NERDTree or fugitive: otherwise the view could only be reached
/// with the mouse or Ctrl-W. Search focuses its own input already.
pub(crate) fn focus_revealed_sidebar(app: &mut App, before: (crate::app::SidebarView, bool)) {
    if !app.vim.enabled || !revealed_sidebar(app, before) {
        return;
    }
    with_vim(app, |vim, host| vim.reset_to_normal(host));
    app.active_pane = ActivePane::Explorer;
    app.terminal_panel_focused = false;
}

/// Command ids `:VT` accepts: the Command Palette's and the menu bar's.
fn palette_ids_of(app: &App) -> Vec<String> {
    let menus = (0..App::menu_labels().len())
        .flat_map(|i| app.menu_items(i))
        .map(|(_, id)| id.to_string());
    let mut seen = std::collections::HashSet::new();
    app.palette
        .commands
        .iter()
        .map(|c| c.id.to_string())
        .chain(menus)
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

/// Toggle Vim mode from the settings or the command palette.
pub(crate) fn set_enabled(app: &mut App, on: bool) {
    if app.vim.enabled == on {
        return;
    }
    if !on {
        with_vim(app, |vim, host| vim.reset_to_normal(host));
        for tab in app
            .tabs
            .iter_mut()
            .chain(app.groups.iter_mut().flat_map(|g| g.tabs.iter_mut()))
        {
            tab.editor.vim_selection = None;
            tab.editor.vim_block = None;
        }
        app.vim.enabled = false;
        app.vim.message = None;
        return;
    }
    app.vim.enabled = true;
    app.vim.mode = Mode::Normal;
    app.vim.last_nav_id = 0;
    load_vimrc(app);
    sync_focus(app);
}

/// The vimrc: `<vtcode-base>/vimrc`, or the path in the `vimrcPath` setting.
pub(crate) fn vimrc_path(app: &App) -> Option<std::path::PathBuf> {
    if let Some(p) = app
        .vim_rc_path
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(p.clone());
    }
    Some(crate::portable::base_dir().join("vimrc"))
}

pub(crate) fn load_vimrc(app: &mut App) {
    let Some(path) = vimrc_path(app) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let errors = with_vim(app, |vim, host| {
        // Start from defaults so reloading does not duplicate state.
        vim.maps = Default::default();
        super::ex::source(vim, host, &text)
    });
    if !errors.is_empty() {
        app.notify_warn(format!(
            "vimrc: {} line(s) not supported ({})",
            errors.len(),
            errors[0]
        ));
    }
}

/// Starting point for a new vimrc.
pub(crate) const VIMRC_TEMPLATE: &str = r#"" VTCode vimrc: read when Vim mode starts (Vim: Reload vimrc re-reads it).
" Supported: set, let mapleader, map/noremap and their n/v/x/o/i/c variants,
" unmap, and any Ex command. Plugin and Vimscript lines are ignored.

let mapleader = " "

" set relativenumber
" set ignorecase smartcase
" set clipboard=unnamedplus
" set scrolloff=8

" inoremap jk <Esc>
" nnoremap <leader>w :w<CR>
" nnoremap <leader>ff :VT quick_open<CR>
" nnoremap <leader>fg :VT global_search<CR>
" nnoremap <leader>e :Explore<CR>
" nnoremap <leader>ca :VT code_action<CR>
" nnoremap <leader>rn :VT rename_symbol<CR>
" nnoremap <C-h> <C-w>h
" nnoremap <C-l> <C-w>l
"#;
