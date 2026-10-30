//! Shared presentation and focus handling for workbench forms and confirmations.
use crate::{
    app::App,
    text_input::TextInputId,
    ui_widgets::{draw_dialog_button, InputField},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use std::path::PathBuf;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    About,
    SaveError,
    FormattingError,
    AgentSettings,
    LspSettings,
    FormatterSettings,
    FileAssociations,
    ExclusionSettings,
    ScopeBubbleSettings,
    CloseAgent,
    ThemeInstall,
    ThemeVsixLink,
    ThemeDelete,
    SaveAs,
    Rename,
    RenameTerminal,
    Goto,
    Close,
    Delete,
    Move,
    Discard,
    StageAllCommit,
    Exit,
    ExitBackup,
    ExitRecovery,
}

impl Kind {
    pub(crate) fn input(self) -> Option<TextInputId> {
        match self {
            Self::ThemeInstall => Some(TextInputId::CommandPalette),
            Self::SaveAs => Some(TextInputId::SaveAs),
            Self::Rename => Some(TextInputId::RenameSymbol),
            Self::RenameTerminal => Some(TextInputId::TerminalRename),
            Self::Goto => Some(TextInputId::GotoLine),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(crate) struct ModalState {
    pub kind: Option<Kind>,
    pub focus: usize,
    pub area: Rect,
    pub close: Rect,
    pub buttons: Vec<(Rect, usize)>,
    pub error: Option<String>,
    pub overwrite: Option<PathBuf>,
    pub mouse_position: (u16, u16),
    pub context_scroll: usize,
    pub context_area: Rect,
    pub context_total: usize,
}

pub(crate) fn active(app: &App) -> Option<Kind> {
    if app.save_error.is_some() {
        return Some(Kind::SaveError);
    }
    if app.exit_recovery.pending() { return Some(Kind::ExitRecovery); }
    if app.change_dir_active {
        return None;
    }
    if app.exit_confirm_active {
        Some(if app.exit_backup_warning.is_some() { Kind::ExitBackup } else { Kind::Exit })
    } else if app.about_info.is_some() {
        Some(Kind::About)
    } else if app.chat.confirm_close_agent.is_some() {
        Some(Kind::CloseAgent)
    } else if app.theme_management.pending.is_some() {
        Some(Kind::ThemeDelete)
    } else if app.confirm_close_active {
        Some(Kind::Close)
    } else if app.confirm_delete_active {
        Some(Kind::Delete)
    } else if app.confirm_move_active {
        Some(Kind::Move)
    } else if app.sc_discard_confirm_active {
        Some(Kind::Discard)
    } else if app.sc_stage_all_commit_request.is_some() {
        Some(Kind::StageAllCommit)
    } else if app.save_as_active {
        Some(Kind::SaveAs)
    } else if app.rename.symbol_active {
        Some(Kind::Rename)
    } else if app.terminal_rename.target.is_some() {
        Some(Kind::RenameTerminal)
    } else if app.goto_line_active {
        Some(Kind::Goto)
    } else if app.formatting.error.is_some() {
        Some(Kind::FormattingError)
    } else if app.scope_bubble_settings.active {
        Some(Kind::ScopeBubbleSettings)
    } else if app.exclusion_settings.active {
        Some(Kind::ExclusionSettings)
    } else if app.file_associations.active {
        Some(Kind::FileAssociations)
    } else if app.formatter_settings.active {
        Some(Kind::FormatterSettings)
    } else if app.lsp_settings.active {
        Some(Kind::LspSettings)
    } else if app.agent_settings.active {
        Some(Kind::AgentSettings)
    } else if app.theme_install_prompt_visible() {
        Some(if app.palette.install_theme_mode && app.palette.theme_vsix_url.is_some() {
            Kind::ThemeVsixLink
        } else {
            Kind::ThemeInstall
        })
    } else {
        None
    }
}

fn sync(app: &mut App, kind: Kind) {
    if app.modal.kind != Some(kind) {
        // An asynchronous dialog interrupts workbench gestures and shortcut
        // prefixes; dismissing it must not resume an obscured interaction.
        app.clear_drag_states();
        app.text_input_drag = None;
        app.drag_start_pos = None;
        app.drag_has_moved = false;
        app.chord_pending = false;
        app.modal = ModalState {
            kind: Some(kind),
            mouse_position: (app.mouse_col, app.mouse_row),
            focus: match kind {
                Kind::About => 1,
                Kind::SaveError if app.save_conflict.is_some() => 1,
                Kind::FormattingError => 1,
                Kind::CloseAgent | Kind::ThemeDelete => 1,
                Kind::ExitBackup => 1,
                Kind::Discard => 1,
                Kind::Delete => app.confirm_delete_selected,
                Kind::Move => app.confirm_move_selected,
                _ => 0,
            },
            ..ModalState::default()
        };
    }
    let count = labels(app, kind).len() + usize::from(kind.input().is_some());
    app.modal.focus = app.modal.focus.min(count - 1);
    if !focus_enabled(app, kind, app.modal.focus) {
        step_focus(app, kind, false, 0);
    }
}

fn focus_enabled(app: &App, kind: Kind, focus: usize) -> bool {
    if kind == Kind::ExitRecovery { return false; }
    if kind == Kind::SaveError && app.save_conflict.as_ref().is_some_and(|conflict| conflict.busy()) { return false; }
    !(kind == Kind::ThemeInstall && app.install_prompt_running() && focus < 2)
}

fn step_focus(app: &mut App, kind: Kind, reverse: bool, first: usize) {
    let count = labels(app, kind).len() + usize::from(kind.input().is_some()) - first;
    for _ in 0..count {
        app.modal.focus = first + (app.modal.focus.saturating_sub(first)
            + if reverse { count - 1 } else { 1 }) % count;
        if focus_enabled(app, kind, app.modal.focus) {
            break;
        }
    }
}

fn labels(app: &App, kind: Kind) -> Vec<&'static str> {
    match kind {
        Kind::About => vec!["Copy Info", "Close"],
        Kind::SaveError if app.save_conflict.is_some() => vec!["Overwrite", "View Diff"],
        Kind::SaveError => vec!["Dismiss"],
        Kind::FormattingError => vec!["Formatter settings…", "Dismiss"],
        Kind::ScopeBubbleSettings => vec!["Done"],
        Kind::ExclusionSettings | Kind::FileAssociations => vec!["Done"],
        Kind::FormatterSettings => vec!["Done"],
        Kind::AgentSettings => vec!["Done"],
        Kind::LspSettings => vec!["Done"],
        Kind::CloseAgent => vec!["Close agent", "Cancel"],
        Kind::ThemeDelete => vec!["Delete theme", "Cancel"],
        Kind::ThemeInstall => {
            let mut buttons = vec![if app.install_prompt_running() {
                if app.palette.install_extension_mode { "Finding versions…" } else { "Installing…" }
            } else {
                if app.palette.install_extension_mode { "Choose version…" }
                else if app.palette.install_syntax_mode { "Install syntax" }
                else { "Install theme" }
            }];
            if app.theme_vsix_link().is_some() {
                buttons.push("Get VSIX link");
            }
            buttons.push("Close");
            buttons
        }
        Kind::ThemeVsixLink => vec!["Copy link", "Open in browser", "Back"],
        Kind::SaveAs => vec![
            if app.modal.overwrite.is_some() {
                "Replace file"
            } else {
                "Save"
            },
            "Browse…",
            "Cancel",
        ],
        Kind::Rename | Kind::RenameTerminal => vec!["Rename", "Cancel"],
        Kind::Goto => vec!["Go to line", "Cancel"],
        Kind::Close => vec!["Save & close", "Don't save", "Cancel"],
        Kind::Delete => vec!["Delete", "Cancel"],
        Kind::Move => vec!["Move", "Cancel"],
        Kind::Discard => vec![if app.sc_discard_request.as_ref().is_some_and(|request| request.all) {
            "Discard all"
        } else { "Discard changes" }, "Cancel"],
        Kind::StageAllCommit => vec!["Stage all & commit", "Cancel"],
        Kind::Exit if app.unsaved_file_count() > 0 => vec![
            "Save & quit",
            "Discard & quit",
            "Keep unsaved & quit",
            "Cancel",
        ],
        Kind::Exit => vec!["Quit", "Cancel"],
        Kind::ExitBackup => vec!["Back up & quit", "Back"],
        Kind::ExitRecovery => vec!["Backing up…"],
    }
}

pub(crate) fn field_changed(app: &mut App) {
    app.modal.error = None;
    app.modal.overwrite = None;
}

fn dismiss(app: &mut App, kind: Kind) {
    match kind {
        Kind::About => app.about_info = None,
        Kind::SaveError => app.dismiss_save_error(),
        Kind::FormattingError => app.formatting.error = None,
        Kind::ScopeBubbleSettings => crate::ui_scope_bubble_settings::activate(app, crate::ui_scope_bubble_settings::Action::Close),
        Kind::ExclusionSettings => crate::ui_exclusion_settings::activate(app, crate::ui_exclusion_settings::Action::Close),
        Kind::FileAssociations => crate::ui_file_associations::activate(app, crate::ui_file_associations::Action::Close),
        Kind::FormatterSettings => crate::ui_formatter_settings::activate(app, crate::ui_formatter_settings::Action::Close),
        Kind::AgentSettings => crate::ui_agent_settings::activate(app, crate::ui_agent_settings::Action::Close),
        Kind::LspSettings => crate::ui_lsp_settings::activate(app, crate::ui_lsp_settings::Action::Close),
        Kind::CloseAgent => app.chat.confirm_close_agent = None,
        Kind::ThemeDelete => app.cancel_theme_removal(),
        Kind::ThemeInstall => app.close_palette(),
        Kind::ThemeVsixLink => app.palette.theme_vsix_url = None,
        Kind::SaveAs => app.save_as_active = false,
        Kind::Rename => app.rename.symbol_active = false,
        Kind::RenameTerminal => app.cancel_terminal_rename(),
        Kind::Goto => app.cancel_goto_line(),
        Kind::Close => app.confirm_close_active = false,
        Kind::Delete => app.execute_confirm_delete(false),
        Kind::Move => app.execute_confirm_move(false),
        Kind::Discard => app.execute_source_control_discard_confirm(false),
        Kind::StageAllCommit => app.sc_stage_all_commit_request = None,
        Kind::Exit => {
            app.exit_confirm_active = false;
            app.exit_backup_warning = None;
        }
        Kind::ExitBackup => {
            app.exit_backup_warning = None;
            // Return to the safe Cancel action rather than letting a repeated
            // Enter immediately save or discard from the original quit dialog.
            app.modal = ModalState { kind: Some(Kind::Exit),
                focus: labels(app, Kind::Exit).len() - 1, ..Default::default() };
            return;
        }
        Kind::ExitRecovery => return,
    }
    app.modal = ModalState::default();
}

fn activate(app: &mut App, kind: Kind, index: usize) -> bool {
    if kind == Kind::SaveError && app.save_conflict.is_some() {
        if !focus_enabled(app, kind, index) { return false; }
        if index == 0 { app.overwrite_save_conflict(); } else { app.view_save_conflict(); }
        return false;
    }
    if index == labels(app, kind).len() - 1 {
        dismiss(app, kind);
        return false;
    }
    match kind {
        Kind::About => {
            if let Some(info) = app.about_info.as_deref() {
                match crate::clipboard::set_text(info) {
                    Ok(()) => {
                        app.modal.error = None;
                        app.notify("About information copied");
                    }
                    Err(error) => app.modal.error = Some(format!("Could not copy information: {error}")),
                }
            }
        }
        Kind::SaveError => {},
        Kind::FormattingError => {
            app.formatting.error = None;
            app.open_formatter_settings(None);
        }
        Kind::FormatterSettings | Kind::FileAssociations | Kind::ExclusionSettings | Kind::ScopeBubbleSettings => {},
        Kind::ThemeInstall if index == 1 => app.show_theme_vsix_link(),
        Kind::ThemeInstall => app.start_theme_install(),
        Kind::ThemeVsixLink if index == 0 => app.copy_theme_vsix_link(),
        Kind::ThemeVsixLink => {
            if let Some(url) = app.palette.theme_vsix_url.clone() {
                app.open_external(crate::external_open::Request::Url(url));
            }
        }
        Kind::AgentSettings => {},
        Kind::LspSettings => {},
        Kind::CloseAgent => app.confirm_close_chat_agent(),
        Kind::ThemeDelete => app.confirm_theme_removal(),
        Kind::SaveAs if index == 1 => {
            let parent = if let Some(remote) = &app.remote_workspace {
                remote.target_key(&app.save_as_query).ok()
                    .and_then(|key| remote.parent_key(&key).ok().flatten())
            } else {
                save_path(app).parent().filter(|path| path.is_dir()).map(|path| path.to_path_buf())
            };
            app.open_folder_browser();
            app.folder_browser.save_as_return = true;
            if let Some(parent) = parent {
                app.navigate_folder_browser(parent);
            }
            app.save_as_active = false;
            app.modal.kind = None;
        }
        Kind::SaveAs => app.execute_save_as(),
        Kind::Rename => {
            if app.rename.symbol_new.trim().is_empty() {
                app.modal.error = Some("Enter a new symbol name.".into());
            } else {
                app.execute_rename_symbol();
            }
        }
        Kind::RenameTerminal => app.confirm_terminal_rename(),
        Kind::Goto => app.goto_line(),
        Kind::Close => crate::input_pickers::handle_confirm_close_key(
            app,
            if index == 0 {
                KeyCode::Char('y')
            } else {
                KeyCode::Char('n')
            },
        ),
        Kind::Delete => app.execute_confirm_delete(true),
        Kind::Move => app.execute_confirm_move(true),
        Kind::Discard => app.execute_source_control_discard_confirm(true),
        Kind::StageAllCommit => app.confirm_stage_all_commit(),
        Kind::ExitBackup => return crate::input_dialogs::confirm_large_backup(app),
        Kind::ExitRecovery => {},
        Kind::Exit => {
            let key = if app.unsaved_file_count() == 0 {
                KeyCode::Char('y')
            } else {
                [KeyCode::Char('y'), KeyCode::Char('d'), KeyCode::Char('k')][index]
            };
            return crate::input_dialogs::handle_exit_confirm_key(
                app,
                KeyEvent::new(key, KeyModifiers::NONE),
            )
            .unwrap_or(false);
        }
    }
    if active(app) != Some(kind) {
        app.modal.kind = None;
    }
    false
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Option<bool> {
    let kind = active(app)?;
    app.chord_pending = false;
    if kind == Kind::ThemeInstall && app.palette.extension_versions.is_some() {
        crate::ui_extension_versions::handle_key(app, key);
        return Some(false);
    }
    if kind == Kind::AgentSettings {
        crate::ui_agent_settings::handle_key(app, key);
        return Some(false);
    }
    if kind == Kind::ScopeBubbleSettings {
        crate::ui_scope_bubble_settings::handle_key(app, key);
        return Some(false);
    }
    if kind == Kind::ExclusionSettings {
        crate::ui_exclusion_settings::handle_key(app, key); return Some(false);
    }
    if kind == Kind::FileAssociations {
        crate::ui_file_associations::handle_key(app, key);
        return Some(false);
    }
    if kind == Kind::FormatterSettings {
        crate::ui_formatter_settings::handle_key(app, key);
        return Some(false);
    }
    if kind == Kind::LspSettings {
        crate::ui_lsp_settings::handle_key(app, key);
        return Some(false);
    }
    sync(app, kind);
    let offset = usize::from(kind.input().is_some());
    match key.code {
        KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End if kind == Kind::ExitBackup => {
            let page = usize::from(app.modal.context_area.height).max(1);
            let maximum = app.modal.context_total.saturating_sub(page);
            app.modal.context_scroll = match key.code {
                KeyCode::Home => 0,
                KeyCode::End => maximum,
                KeyCode::PageUp => app.modal.context_scroll.saturating_sub(page),
                _ => app.modal.context_scroll.saturating_add(page).min(maximum),
            };
        }
        KeyCode::Char('c' | 'C') if kind == Kind::About && key.modifiers.contains(KeyModifiers::CONTROL) => {
            activate(app, kind, 0);
        }
        KeyCode::Esc => {
            dismiss(app, kind);
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let reverse =
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            step_focus(app, kind, reverse, 0);
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down if app.modal.focus >= offset => {
            let reverse = matches!(key.code, KeyCode::Left | KeyCode::Up);
            step_focus(app, kind, reverse, offset);
        }
        KeyCode::Up | KeyCode::Down => {
            step_focus(app, kind, key.code == KeyCode::Up, 0);
        }
        KeyCode::Enter => {
            return Some(activate(app, kind, app.modal.focus.saturating_sub(offset)));
        }
        KeyCode::Char(' ') if app.modal.focus >= offset => {
            return Some(activate(app, kind, app.modal.focus - offset));
        }
        _ if offset == 1
            && app.modal.focus == 0
            && !(kind == Kind::ThemeInstall && app.install_prompt_running()) =>
        {
            let id = kind.input().unwrap();
            if app
                .edit_text_input(id, key.code, key.modifiers, None)
                .changed
            {
                app.text_input_changed(id);
            }
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            if kind == Kind::Exit && matches!(c.to_ascii_lowercase(), 'y' | 'd' | 'k' | 'n') {
                return crate::input_dialogs::handle_exit_confirm_key(app, key);
            }
            let index = match (kind, c.to_ascii_lowercase()) {
                (Kind::Close, 'y') => Some(0),
                (Kind::Close, 'n') => Some(1),
                _ => None,
            };
            if let Some(index) = index {
                return Some(activate(app, kind, index));
            }
        }
        _ => {}
    }
    if kind == Kind::Delete {
        app.confirm_delete_selected = app.modal.focus;
    }
    if kind == Kind::Move {
        app.confirm_move_selected = app.modal.focus;
    }
    Some(false)
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let Some(kind) = active(app) else {
        return false;
    };
    if kind == Kind::ThemeInstall && app.palette.extension_versions.is_some() {
        crate::ui_extension_versions::handle_mouse(app, mouse);
        return true;
    }
    if kind == Kind::AgentSettings {
        crate::ui_agent_settings::handle_mouse(app, mouse);
        return true;
    }
    if kind == Kind::ScopeBubbleSettings {
        crate::ui_scope_bubble_settings::handle_mouse(app, mouse);
        return true;
    }
    if kind == Kind::ExclusionSettings {
        crate::ui_exclusion_settings::handle_mouse(app, mouse); return true;
    }
    if kind == Kind::FileAssociations {
        crate::ui_file_associations::handle_mouse(app, mouse);
        return true;
    }
    if kind == Kind::FormatterSettings {
        crate::ui_formatter_settings::handle_mouse(app, mouse);
        return true;
    }
    if kind == Kind::LspSettings {
        crate::ui_lsp_settings::handle_mouse(app, mouse);
        return true;
    }
    sync(app, kind);
    if kind == Kind::ExitBackup && matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
        let maximum = app.modal.context_total.saturating_sub(usize::from(app.modal.context_area.height));
        app.modal.context_scroll = if mouse.kind == MouseEventKind::ScrollUp {
            app.modal.context_scroll.saturating_sub(3)
        } else { app.modal.context_scroll.saturating_add(3).min(maximum) };
        return true;
    }
    let position = (mouse.column, mouse.row);
    let moved = app.modal.mouse_position != position;
    app.modal.mouse_position = position;
    if moved && mouse.kind == MouseEventKind::Moved
        && let Some((_, index)) = app.modal.buttons.iter().find(|(area, index)| {
            area.contains(position.into())
                && focus_enabled(app, kind, index + usize::from(kind.input().is_some()))
        })
    {
        app.modal.focus = index + usize::from(kind.input().is_some());
        if kind == Kind::Delete {
            app.confirm_delete_selected = app.modal.focus;
        }
        if kind == Kind::Move {
            app.confirm_move_selected = app.modal.focus;
        }
    }
    if app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers) {
        return true;
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let point = (mouse.column, mouse.row).into();
        if app.modal.close.contains(point)
            || (!app.modal.area.is_empty() && !app.modal.area.contains(point))
        {
            dismiss(app, kind);
        } else if let Some((_, index)) = app
            .modal
            .buttons
            .iter()
            .find(|(area, _)| area.contains(point))
            .copied()
        {
            let focus = index + usize::from(kind.input().is_some());
            if !focus_enabled(app, kind, focus) {
                return true;
            }
            app.modal.focus = focus;
            if kind == Kind::Exit {
                // Use the keyboard's save/pending-operation policy too. The
                // old mouse path hid this dialog and left quit flags waiting
                // forever, with neither an error nor a usable Cancel button.
                if activate(app, kind, index) {
                    if index == 0 { app.exit_save_and_quit = true; }
                    app.exit_confirm_active = false;
                    app.modal.kind = None;
                }
            } else {
                activate(app, kind, index);
            }
        }
    }
    true
}

fn save_path(app: &App) -> PathBuf {
    let input = app.save_as_query.trim().trim_matches('"');
    let path = PathBuf::from(input);
    if path.is_absolute() {
        path
    } else {
        app.explorer.root.join(path)
    }
}

/// Validate before the existing background save path changes the document's filename.
pub(crate) fn save_target(app: &mut App) -> Option<(PathBuf, bool)> {
    sync(app, Kind::SaveAs);
    if let Some(remote) = &app.remote_workspace {
        let key = match remote.target_key(&app.save_as_query) {
            Ok(key) => key,
            Err(error) => { app.modal.error = Some(error.to_string()); return None; }
        };
        let Some(editor) = app.editor() else { app.modal.error = Some("Open a document before saving.".into()); return None; };
        let identity = editor.save_identity;
        if let Some(error) = app.editor().and_then(crate::editor::Editor::save_content_error) {
            app.modal.error = Some(error.into()); return None;
        }
        if app.tabs.iter().chain(app.groups.iter().flat_map(|g| &g.tabs)).any(|tab|
            tab.kind == crate::app::TabKind::File && tab.editor.file_path.as_ref() == Some(&key)
                && tab.editor.save_identity != identity) {
            app.modal.error = Some("This remote file is already open. Choose another name.".into()); return None;
        }
        // Existence and permissions are checked atomically by the service, not
        // with a network round trip or a native Windows metadata call here.
        return Some((key, false));
    }
    let path = save_path(app);
    let error = if app.save_as_query.trim().is_empty() {
        Some("Enter a file name or path.")
    } else if path.is_dir()
        || path.file_name().is_none()
        || app.save_as_query.trim().ends_with(['/', '\\'])
    {
        Some("This is a folder. Add a file name to the path.")
    } else if !path.parent().is_some_and(|p| p.is_dir()) {
        Some("The destination folder does not exist. Choose Browse to select one.")
    } else if app.editor().is_none() {
        Some("Open a document before saving.")
    } else if app.active_extension_view().is_none() && app.editor().is_some_and(|editor|
        app.tabs.iter().chain(app.groups.iter().flat_map(|g| &g.tabs)).any(|tab|
            tab.kind == crate::app::TabKind::File && tab.editor.save_identity != editor.save_identity
                && tab.editor.file_path.as_ref().is_some_and(|other| App::same_open_path(other, &path))))
    {
        Some("This file is already open. Close it before replacing it, or choose another name.")
    } else {
        app.editor().and_then(crate::editor::Editor::save_content_error)
    };
    if let Some(error) = error {
        app.modal.error = Some(error.into());
        return None;
    }
    let replace = app.modal.overwrite.as_ref() == Some(&path);
    if path.exists() && !replace {
        app.modal.overwrite = Some(path);
        app.modal.error = None;
        return None;
    }
    app.modal.overwrite = None;
    Some((path, replace))
}

struct Copy {
    title: &'static str,
    message: String,
    context: Vec<String>,
    help: String,
    field: &'static str,
}

fn document_name(editor: &crate::editor::Editor) -> std::borrow::Cow<'_, str> {
    editor.display_name()
}

fn copy(app: &App, kind: Kind) -> Copy {
    match kind {
        Kind::About => Copy {
            title: "About VTCode",
            message: app.about_info.clone().unwrap_or_default(),
            context: Vec::new(),
            help: String::new(),
            field: "",
        },
        Kind::SaveError if app.save_conflict.is_some() => Copy {
            title: "File changed outside VTCode",
            message: "This file was edited outside VTCode. Overwrite will replace the version on disk with your local edits and lose the external changes.".into(),
            context: vec![app.save_conflict_path().unwrap_or_default()],
            help: if app.save_conflict.as_ref().is_some_and(|conflict| conflict.busy()) {
                "Loading comparison… Esc to cancel.".into()
            } else { "View Diff compares the current disk file with your unsaved edits. Esc to cancel.".into() },
            field: "",
        },
        Kind::SaveError => Copy {
            title: "Save failed",
            message: app.save_error.clone().unwrap_or_default(),
            context: Vec::new(),
            help: "Your edits remain open. Esc to dismiss.".into(),
            field: "",
        },
        Kind::FormattingError => Copy {
            title: "Formatting failed",
            message: app.formatting.error.clone().unwrap_or_default(),
            context: Vec::new(),
            help: "Esc to dismiss · Formatter settings to choose or configure a formatter".into(),
            field: "",
        },
        Kind::AgentSettings => Copy {
            title: "Agent Chat", message: String::new(), context: Vec::new(), help: String::new(), field: "",
        },
        Kind::LspSettings => Copy {
            title: "Language Servers", message: String::new(), context: Vec::new(), help: String::new(), field: "",
        },
        Kind::ScopeBubbleSettings => Copy { title: "Scope Bubble", message: String::new(), context: Vec::new(), help: String::new(), field: "" },
        Kind::ExclusionSettings => Copy { title: "Exclusions", message: String::new(), context: Vec::new(), help: String::new(), field: "" },
        Kind::FileAssociations => Copy {
            title: "File Associations", message: String::new(), context: Vec::new(), help: String::new(), field: "",
        },
        Kind::FormatterSettings => Copy {
            title: "Formatters", message: String::new(), context: Vec::new(), help: String::new(), field: "",
        },
        Kind::CloseAgent => {
            let agent = app.chat.confirm_close_agent.unwrap_or(app.chat.active_agent);
            let state = app.chat.agent_state(agent);
            let name: String = state.map_or("agent", |state| state.tab_name()).chars().filter(|c| !c.is_control()).collect();
            let count = app.chat.agent_open_conversations(agent);
            let running = state.map_or(0, |state| state.sessions.values().filter(|session| session.busy).count());
            let queued = state.map_or(0, |state| state.queues.values().map(|queue| queue.items.len() + usize::from(queue.inflight.is_some())).sum::<usize>());
            let mut context = vec![format!("{count} open conversation{}", if count == 1 { "" } else { "s" })];
            let editors = app.chat.layout.editors.values().filter(|view| view.agent == agent).count();
            if editors > 0 { context.push(format!("Including {editors} in editor tabs")); }
            if running > 0 { context.push(format!("{running} running response{}", if running == 1 { "" } else { "s" })); }
            if queued > 0 { context.push(format!("{queued} queued message{}", if queued == 1 { "" } else { "s" })); }
            Copy {
                title: "Close Agent",
                message: format!("Close {name}?"),
                context,
                help: "Open conversations will close and running work will stop. Unsent drafts and queued messages will be lost. Agent-saved history is not deleted.".into(),
                field: "",
            }
        }
        Kind::ThemeDelete => {
            let pending = app.theme_management.pending.as_ref().expect("theme removal dialog");
            let name: String = pending.name.chars().filter(|c| !c.is_control()).collect();
            let mut context = vec![pending.key.chars().filter(|c| !c.is_control()).collect()];
            if let Some(key) = &pending.fallback {
                if let Some(theme) = crate::app_theme_loader::builtin_theme(key) {
                    context.push(format!("Current theme will switch to {}.", theme.name));
                }
            }
            Copy {
                title: "Delete Theme",
                message: format!("Delete \"{name}\" from your installed themes?"),
                context,
                help: "Only this variant is removed. Other themes and shared pack files are kept. Reinstall the theme pack to restore it.".into(),
                field: "",
            }
        }
        Kind::ThemeInstall if app.palette.install_syntax_mode => Copy {
            title: "Install Syntax",
            message: "Add a language for syntax highlighting.".into(),
            context: vec!["TextMate (.tmLanguage.json or .tmLanguage)".into()],
            help: if app.install_prompt_running() {
                "Validating and installing syntax…".into()
            } else {
                "Enter a local file path. URLs are not supported. Highlighting reloads automatically.".into()
            },
            field: "Syntax file path",
        },
        Kind::ThemeInstall if app.palette.install_extension_mode => Copy {
            title: "Install Extension",
            message: "Add an extension from a published GitHub release.".into(),
            context: vec!["GitHub release package · Version tag required".into()],
            help: if app.palette.theme_install_running {
                "Finding published GitHub release packages…".into()
            } else {
                "Paste a GitHub repository link, then choose a packaged release tag. Repositories without published VTCode release packages cannot be installed.".into()
            },
            field: "GitHub repository link",
        },
        Kind::ThemeInstall => Copy {
            title: "Install Color Theme",
            message: "Add a color theme to your workbench.".into(),
            context: vec!["vscodethemes.com link, VSIX, ZIP, JSON, JSONC or folder".into()],
            help: if app.palette.theme_install_running {
                if app.palette.query.contains("://") {
                    "Downloading and installing theme variants…".into()
                } else {
                    "Installing theme variants…".into()
                }
            } else if app.theme_vsix_link().is_some() {
                "Install all theme variants, or choose Get VSIX link to download the package in your browser.".into()
            } else {
                "Paste a theme link or choose a local package. All included variants will be available in the theme picker.".into()
            },
            field: "Theme link or local path",
        },
        Kind::ThemeVsixLink => Copy {
            title: "Theme VSIX Download Link",
            message: app.palette.theme_vsix_url.clone().unwrap_or_default(),
            context: Vec::new(),
            help: "Download in your browser, or copy the link to use on another device. Then choose Back and paste the downloaded .vsix file path to install.".into(),
            field: "",
        },
        Kind::SaveAs => Copy {
            title: "Save As",
            message: "Choose a name and location for this file.".into(),
            context: vec![
                format!(
                    "Document   {}",
                    app.editor().map_or("Untitled".into(), document_name)
                ),
                format!("Workspace  {}", app.explorer.root.display()),
            ],
            help: "Relative paths are saved inside this workspace.".into(),
            field: "File name or full path",
        },
        Kind::Rename => Copy {
            title: "Rename Symbol",
            message: "Give this symbol a new name.".into(),
            context: vec![format!("Current name   {}", app.rename.symbol_old)],
            help: "Uses language-server rename when available; otherwise renames in this file."
                .into(),
            field: "New name",
        },
        Kind::RenameTerminal => Copy {
            title: "Rename Terminal",
            message: "Choose a name for this terminal tab.".into(),
            context: app.terminal_rename.target.and_then(|index| app.terminals.get(index))
                .map(|terminal| vec![format!("Current name   {}", terminal.name)]).unwrap_or_default(),
            help: String::new(),
            field: "New name",
        },
        Kind::Goto => Copy {
            title: "Go to Line",
            message: "Jump to a position in the current file.".into(),
            context: vec![app.editor().map_or("No document open".into(), |e| {
                format!("{}  ·  {} lines", document_name(e), e.line_count())
            })],
            help: "Enter a line or line:column. Escape restores your original position.".into(),
            field: "Line : column",
        },
        Kind::Close => Copy {
            title: "Unsaved Changes",
            message: "Save your changes before closing?".into(),
            context: vec![app
                .tabs
                .get(app.confirm_close_tab)
                .map_or("Untitled".into(), |t| {
                    t.editor.file_path.as_ref().map_or_else(
                        || document_name(&t.editor).into(),
                        |p| p.display().to_string(),
                    )
                })],
            help: "If you don't save, changes to this document will be lost.".into(),
            field: "",
        },
        Kind::Delete => {
            let targets = if app.confirm_delete_targets.is_empty() {
                app.confirm_delete_target.as_slice()
            } else {
                app.confirm_delete_targets.as_slice()
            };
            let count = targets.len().max(1);
            let mut context: Vec<_> = targets
                .iter()
                .take(3)
                .map(|p| app.display_path(p))
                .collect();
            if count > 3 {
                context.push(format!("… and {} more items", count - 3));
            }
            Copy {
                title: "Delete",
                message: if count == 1 {
                    "Delete this item?".into()
                } else {
                    format!("Delete these {count} files or folders?")
                },
                context,
                help: "Items will be permanently deleted. This cannot be undone.".into(),
                field: "",
            }
        }
        Kind::Move => Copy {
            title: "Move",
            message: "Move this item to its new location?".into(),
            context: vec![
                format!(
                    "From   {}",
                    app.confirm_move_source
                        .as_ref()
                        .map_or(String::new(), |p| p.display().to_string())
                ),
                format!(
                    "To     {}",
                    app.confirm_move_destination
                        .as_ref()
                        .map_or(String::new(), |p| p.display().to_string())
                ),
            ],
            help: "The item will appear in the destination folder.".into(),
            field: "",
        },
        Kind::Discard => {
            let request = app.sc_discard_request.as_ref();
            let all = request.is_some_and(|request| request.all);
            let changes = request.map_or(&[][..], |request| request.changes.as_slice());
            let mut context: Vec<_> = changes.iter().take(4).map(|file| file.path.clone()).collect();
            if changes.len() > 4 { context.push(format!("… and {} more files", changes.len() - 4)); }
            let mut help = if changes.iter().any(|file| file.staged) {
                "Staged and unstaged edits to these files will be removed. ".to_string()
            } else { "Staged changes will be kept. ".to_string() };
            if changes.iter().any(|file| file.status == '?' || file.status == 'A' && file.staged) {
                help.push_str("New files will be permanently deleted. ");
            }
            help.push_str("This cannot be undone.");
            Copy {
                title: if all { "Discard All Changes" } else { "Discard Changes" },
                message: format!("Discard {}changes in {} file{}?", if all { "all unstaged " } else { "selected " },
                    changes.len(), if changes.len() == 1 { "" } else { "s" }),
                context, help, field: "",
            }
        },
        Kind::StageAllCommit => Copy {
            title: "Stage All & Commit",
            message: "No changes are staged. Stage all changes and commit?".into(),
            context: app.sc_stage_all_commit_request.as_ref().map_or_else(
                Vec::new,
                |request| vec![
                    format!("Workspace  {}", request.root.display()),
                    format!("Message    {}", request.message.lines().next().unwrap_or_default()),
                ],
            ),
            help: "Stages saved modifications, deletions, and new files using Git's ignore rules.".into(),
            field: "",
        },
        Kind::ExitBackup => {
            let warning = app.exit_backup_warning.as_ref().expect("active backup warning");
            Copy {
                title: "Back up large unsaved files?",
                message: format!("At least {} of recovery storage is needed for {} large unsaved file{}.",
                    crate::process_usage::format_bytes(warning.total_bytes),
                    warning.files.len(), if warning.files.len() == 1 { "" } else { "s" }),
                context: warning.files.iter().map(|file| {
                    let mut label = format!("{} · {}", file.name, crate::process_usage::format_bytes(file.bytes));
                    if file.history_bytes > file.bytes {
                        label.push_str(&format!(" · undo up to {}", crate::process_usage::format_bytes(file.history_bytes)));
                    }
                    label
                }).collect(),
                help: "Recovery copies preserve your edits for next time; original files stay unsaved. Undo history and other unsaved files may use additional space. Scroll or PgUp/PgDn to review the list.".into(),
                field: "",
            }
        }
        Kind::ExitRecovery => Copy {
            title: "Backing up unsaved files…",
            message: "Writing recovery data on this computer. VTCode will quit when the backup is complete.".into(),
            context: Vec::new(),
            help: "Your edits will stay open if the backup cannot be completed.".into(),
            field: "",
        },
        Kind::Exit => {
            let count = app.unsaved_file_count();
            let mut context: Vec<_> = app
                .tabs
                .iter()
                .chain(app.groups.iter().flat_map(|g| &g.tabs))
                .filter(|t| t.has_unsaved_changes())
                .take(3)
                .map(|t| document_name(&t.editor).to_string())
                .collect();
            if count > 3 {
                context.push(format!("… and {} more documents", count - 3));
            }
            Copy {
                title: "Quit VT Code",
                message: if count == 0 {
                    "Close this workspace and quit?".into()
                } else {
                    format!(
                        "You have {count} unsaved document{}.",
                        if count == 1 { "" } else { "s" }
                    )
                },
                context,
                help: if count == 0 {
                    "Your workspace can be restored the next time you open VT Code.".into()
                } else {
                    "Keep unsaved & quit preserves your edits in the session for next time.".into()
                },
                field: "",
            }
        }
    }
}

fn text(f: &mut Frame, area: Rect, value: &str, style: Style) {
    if !area.is_empty() {
        f.render_widget(
            Paragraph::new(value)
                .style(style)
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

fn path_tail(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut tail = String::new();
    let mut used = 1;
    for ch in value.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        tail.insert(0, ch);
    }
    format!("…{tail}")
}

fn wrapped_height(value: &str, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let mut lines = 0usize;
    // Preserve hard breaks and blank rows before measuring soft wrapping.
    for line in value.split('\n') {
        lines += 1;
        let mut used = 0;
        for word in line.split_whitespace() {
            let length = word.width();
            if used > 0 && used + 1 + length > width {
                lines += 1;
                used = 0;
            }
            used += usize::from(used > 0) + length;
            if used > width {
                lines += (used - 1) / width;
                used = (used - 1) % width + 1;
            }
        }
    }
    lines.min(u16::MAX as usize) as u16
}

pub(crate) fn draw_active(f: &mut Frame, app: &mut App) {
    match active(app) {
        Some(Kind::About) => draw(f, app, Kind::About),
        Some(Kind::SaveError) => draw(f, app, Kind::SaveError),
        Some(Kind::FormattingError) => draw(f, app, Kind::FormattingError),
        Some(Kind::AgentSettings) => crate::ui_agent_settings::draw(f, app),
        Some(Kind::LspSettings) => crate::ui_lsp_settings::draw(f, app),
        Some(Kind::ScopeBubbleSettings) => crate::ui_scope_bubble_settings::draw(f, app),
        Some(Kind::ExclusionSettings) => crate::ui_exclusion_settings::draw(f, app),
        Some(Kind::FileAssociations) => crate::ui_file_associations::draw(f, app),
        Some(Kind::FormatterSettings) => crate::ui_formatter_settings::draw(f, app),
        Some(Kind::CloseAgent) => draw(f, app, Kind::CloseAgent),
        Some(Kind::ThemeInstall) => draw(f, app, Kind::ThemeInstall),
        Some(Kind::ThemeVsixLink) => draw(f, app, Kind::ThemeVsixLink),
        Some(Kind::ThemeDelete) => draw(f, app, Kind::ThemeDelete),
        Some(Kind::SaveAs) => crate::ui_dialogs::draw_save_as_dialog(f, app),
        Some(Kind::Rename) => crate::ui_dialogs::draw_rename_symbol_dialog(f, app),
        Some(Kind::RenameTerminal) => draw(f, app, Kind::RenameTerminal),
        Some(Kind::Goto) => crate::ui_search_dialog::draw_goto_line(f, app),
        Some(Kind::Close) => crate::ui_dialogs::draw_confirm_close(f, app),
        Some(Kind::Delete) => crate::ui_dialogs::draw_confirm_delete(f, app),
        Some(Kind::Move) => crate::ui_dialogs::draw_confirm_move(f, app),
        Some(Kind::Discard) => crate::ui_dialogs::draw_source_control_discard_confirm(f, app),
        Some(Kind::StageAllCommit) => draw(f, app, Kind::StageAllCommit),
        Some(Kind::Exit) => crate::ui_dialogs::draw_exit_confirm(f, app),
        Some(Kind::ExitBackup) => draw(f, app, Kind::ExitBackup),
        Some(Kind::ExitRecovery) => draw(f, app, Kind::ExitRecovery),
        None => app.modal.kind = None,
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, kind: Kind) {
    if kind == Kind::ThemeInstall && app.palette.extension_versions.is_some() {
        crate::ui_extension_versions::draw(f, app);
        return;
    }
    sync(app, kind);
    app.text_input_regions.clear();
    app.modal.buttons.clear();
    let data = copy(app, kind);
    if kind == Kind::ThemeInstall {
        app.modal.error = if app.install_prompt_running() {
            None
        } else {
            app.palette.theme_install_error.clone()
        };
    }
    let viewport = f.area();
    let width = 76.min(
        viewport
            .width
            .saturating_sub(if viewport.width >= 50 { 6 } else { 0 }),
    );
    let content_width = width.saturating_sub(6).max(1);
    let notice = app.modal.error.clone().unwrap_or_else(|| {
        if app.modal.overwrite.is_some() {
            "A file already exists at this path. Choose Replace file to overwrite it.".into()
        } else {
            data.help.clone()
        }
    });
    let message_lines = wrapped_height(&data.message, content_width).min(viewport.height);
    let notice_lines = wrapped_height(&notice, content_width).min(viewport.height);
    let labels = labels(app, kind);
    let mut button_positions = Vec::new();
    let mut used = 0;
    let mut button_rows = 1;
    for label in &labels {
        let w = (label.width() as u16 + 4).min(content_width);
        if used > 0 && used + w > content_width {
            button_rows += 1;
            used = 0;
        }
        button_positions.push((used, button_rows - 1, w));
        used += w + 1;
    }
    let has_input = kind.input().is_some();
    let context_lines = data.context.len().min(if kind == Kind::ExitBackup { 12 } else { u16::MAX as usize }) as u16;
    let desired = 7u16
        .saturating_add(context_lines)
        .saturating_add(message_lines)
        .saturating_add(notice_lines)
        .saturating_add(if has_input { 4 } else { 0 })
        .saturating_add(button_rows);
    let height = desired.min(
        viewport
            .height
            .saturating_sub(u16::from(viewport.height >= 14) * 2),
    );
    let area = Rect::new(
        viewport.x + viewport.width.saturating_sub(width) / 2,
        viewport.y + viewport.height.saturating_sub(height) / 2,
        width,
        height,
    );
    app.modal.area = area;
    // Dim the workbench without painting another application window.
    for cell in &mut f.buffer_mut().content {
        cell.modifier.insert(Modifier::DIM);
    }
    let bg = crate::ui_widgets::widget_background(f, &app.colors, area);
    f.render_widget(Clear, area);
    let fg = app.colors.ui.widget_text_color(bg);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.colors.ui.widget_border.border(bg).unwrap_or(bg)))
        .style(
            Style::default()
                .bg(bg)
                .fg(fg),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let padding = 2.min(inner.width / 4);
    let padded = Rect::new(
        inner.x + padding,
        inner.y,
        inner.width.saturating_sub(padding * 2),
        inner.height,
    );
    let mut lengths = [
        2,
        message_lines.saturating_add(1),
        context_lines.saturating_add(1),
        if has_input { 4 } else { 0 },
        notice_lines.saturating_add(1),
        button_rows,
    ];
    let mut shortfall = desired.saturating_sub(height);
    // Remove optional spacing first, then compact the field and supporting copy.
    // A slightly shorter viewport should not collapse every section at once.
    for (index, minimum) in [
        (0, 1),
        (2, context_lines),
        (1, message_lines),
        (4, notice_lines),
        (3, if has_input { 2 } else { 0 }),
        (2, context_lines.min(1)),
        (1, 1),
        (4, 1),
    ] {
        let reduction = lengths[index].saturating_sub(minimum).min(shortfall);
        lengths[index] -= reduction;
        shortfall -= reduction;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(lengths.map(Constraint::Length))
        .split(padded);
    let title = Rect::new(inner.x, inner.y, inner.width, rows[0].height.min(1));
    text(
        f,
        title,
        &format!("  {}", if kind == Kind::ExitBackup && width < 40 { "Large unsaved files" }
            else if kind == Kind::ExitRecovery && width < 40 { "Backing up…" } else { data.title }),
        Style::default()
            .fg(fg)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    );
    app.modal.close = Rect::new(
        title.right().saturating_sub(3).max(title.x),
        title.y,
        3.min(title.width),
        title.height,
    );
    if kind == Kind::ExitRecovery { app.modal.close = Rect::default(); }
    let close_hover = app
        .modal
        .close
        .contains((app.mouse_col, app.mouse_row).into());
    text(
        f,
        app.modal.close,
        " × ",
        app.colors.ui.toolbar_layer_style(app.colors.ui.widget_foreground, bg, close_hover, false, false),
    );
    text(
        f,
        rows[1],
        &data.message,
        Style::default()
            .fg(fg)
            .add_modifier(if kind == Kind::About { Modifier::empty() } else { Modifier::BOLD }),
    );
    app.modal.context_area = rows[2];
    app.modal.context_total = data.context.len();
    let scrollable = kind == Kind::ExitBackup && data.context.len() > rows[2].height as usize;
    app.modal.context_scroll = app.modal.context_scroll.min(data.context.len().saturating_sub(rows[2].height as usize));
    for (i, line) in data
        .context
        .iter()
        .skip(app.modal.context_scroll)
        .take(rows[2].height as usize)
        .enumerate()
    {
        let row = Rect::new(rows[2].x, rows[2].y + i as u16, rows[2].width.saturating_sub(u16::from(scrollable)), 1);
        let value = format!(" {}", path_tail(line, row.width.saturating_sub(2) as usize));
        text(
            f,
            row,
            &value,
            Style::default()
                .fg(fg)
                .bg(bg),
        );
    }
    if scrollable && !rows[2].is_empty() {
        let (track, thumb) = app.colors.ui.scrollbar_colors(bg, false, false);
        crate::ui_widgets::VerticalScrollbar::new(rows[2].right() - 1, rows[2].y,
            rows[2].height, data.context.len(), app.modal.context_scroll)
            .symbols('┃', '│').colors(thumb, track, bg).render(f);
    }
    if let Some(id) = kind.input() {
        let field_area = rows[3];
        text(
            f,
            Rect::new(
                field_area.x,
                field_area.y,
                field_area.width,
                field_area.height.min(1),
            ),
            data.field,
            Style::default().fg(app.colors.ui.description_color(bg)),
        );
        let input_area = Rect::new(
            field_area.x,
            field_area.y + field_area.height.min(1),
            field_area.width,
            field_area.height.saturating_sub(1),
        );
        let focused = app.modal.focus == 0;
        let input_style = app.colors.ui.input_style(bg);
        let input_bg = input_style.bg.unwrap();
        let input_fg = input_style.fg.unwrap();
        let border = Block::default()
            .borders(if input_area.height >= 3 {
                Borders::ALL
            } else {
                Borders::NONE
            })
            .border_style(Style::default().fg((if focused { app.colors.ui.focus_border }
                else { app.colors.ui.input_colors.border }).border(input_bg).unwrap_or(input_bg)))
            .style(input_style);
        let input_inner = border.inner(input_area);
        f.render_widget(border, input_area);
        let (value, cursor, anchor) = match id {
            TextInputId::CommandPalette => (
                &app.palette.query,
                app.palette.cursor,
                app.palette.selection_anchor,
            ),
            TextInputId::SaveAs => (
                &app.save_as_query,
                app.save_as_cursor,
                app.save_as_selection_anchor,
            ),
            TextInputId::RenameSymbol => (
                &app.rename.symbol_new,
                app.rename.symbol_cursor,
                app.rename.symbol_selection_anchor,
            ),
            TextInputId::TerminalRename => (
                &app.terminal_rename.query,
                app.terminal_rename.cursor,
                app.terminal_rename.anchor,
            ),
            _ => (
                &app.goto_line_query,
                app.goto_line_cursor,
                app.goto_line_selection_anchor,
            ),
        };
        let mut field = InputField::new(value)
        .theme_colors(&app.colors)
            .prefix(" ", input_fg)
            .bg(input_bg)
            .placeholder_text(
                match kind {
                    Kind::Goto => "e.g. 42:8",
                    Kind::ThemeInstall => if app.palette.install_syntax_mode { "Path to a syntax file…" }
                        else if app.palette.install_extension_mode { "https://github.com/owner/extensions…" }
                        else { "vscodethemes.com link or local path…" },
                    Kind::SaveAs => "e.g. src/main.rs",
                    Kind::Rename => "New symbol name",
                    Kind::RenameTerminal => "Terminal name",
                    _ => "Enter a value",
                },
            )
            .cursor_marker(if focused { "│" } else { "" })
            .cursor_pos(cursor.min(value.len()));
        if focused {
            field = field.selection_range(
                App::inline_selection_range(cursor, anchor),
            );
        }
        if !input_inner.is_empty() {
            let hit = field.render(f, input_inner);
            app.register_text_input(id, hit);
        }
    }
    let caution = matches!(kind, Kind::Delete | Kind::Discard | Kind::CloseAgent | Kind::ThemeDelete) || app.modal.overwrite.is_some();
    text(
        f,
        rows[4],
        &notice,
        Style::default().fg(if app.modal.error.is_some() {
            app.colors.ui.error_text_color(bg)
        } else if caution {
            app.colors.ui.warning_text_color(bg)
        } else {
            app.colors.ui.description_color(bg)
        }),
    );
    for (i, (label, &(x, y, width))) in labels.iter().zip(&button_positions).enumerate() {
        if y >= rows[5].height {
            continue;
        }
        let row_width = button_positions
            .iter()
            .filter(|(_, ry, _)| *ry == y)
            .map(|(rx, _, w)| rx + w)
            .max()
            .unwrap_or(0);
        let x = rows[5].x + rows[5].width.saturating_sub(row_width) + x;
        let rect = Rect::new(
            x.min(rows[5].right()),
            rows[5].y + y,
            width.min(rows[5].right().saturating_sub(x)),
            1,
        );
        let selected = app.modal.focus == i + usize::from(has_input);
        let destructive = (matches!(kind, Kind::Delete | Kind::Discard | Kind::CloseAgent | Kind::ThemeDelete) && i == 0)
            || (kind == Kind::SaveError && app.save_conflict.is_some() && i == 0)
            || (matches!(kind, Kind::Close | Kind::Exit) && i == 1 && i + 1 < labels.len());
        let disabled = (kind == Kind::ThemeInstall && app.install_prompt_running() && i == 0)
            || kind == Kind::ExitRecovery
            || (kind == Kind::SaveError && app.save_conflict.as_ref().is_some_and(|conflict| conflict.busy()));
        draw_dialog_button(f, rect, label, &app.colors, i == 0, selected, destructive, !disabled);
        if !rect.is_empty() {
            app.modal.buttons.push((rect, i));
        }
    }
    let positions = app
        .modal
        .buttons
        .iter()
        .map(|(r, _)| (r.x, r.y, r.width))
        .collect();
    match kind {
        Kind::Close => app.confirm_close_btn_areas = positions,
        Kind::Delete => app.confirm_delete_btn_areas = positions,
        Kind::Move => app.confirm_move_btn_areas = positions,
        Kind::Discard => app.sc_discard_confirm_btn_areas = positions,
        Kind::Exit | Kind::ExitBackup => app.exit_confirm_buttons = positions,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Tab, editor::Editor};
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::Duration;

    fn app() -> App {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(
            "one\ntwo\nthree\nfour\n",
        ))];
        app.active_tab = 0;
        app
    }

    #[test]
    fn about_opens_from_palette_and_keyboard_or_mouse_help_menu_in_all_editor_contexts() {
        assert!(App::build_command_list_with_themes(&[], &[])
            .iter().any(|command| command.id == "about" && command.name == "Help: About"));
        for context in 0..4 {
            for route in 0..3 {
                let mut app = app();
                match context {
                    1 => app.tabs.clear(),
                    2 => { app.execute_palette_command("settings"); }
                    3 => app.show_output(),
                    _ => {}
                }
                let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
                if route == 0 {
                    app.execute_palette_command("about");
                } else {
                    app.menu_active = true;
                    app.menu_open = true;
                    app.menu_selected = 7;
                    app.menu_item_selected = app.menu_items(7).iter()
                        .position(|(_, id)| *id == "about").unwrap();
                    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                    if route == 1 {
                        crate::input_keyboard::handle_keyboard_event(
                            &mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    } else {
                        let x = crate::ui_chrome::menu_dropdown_anchor_x(&app) + 2;
                        let y = app.menu_bar_area.unwrap().y + 2 + app.menu_item_selected as u16;
                        click(&mut app, Rect::new(x, y, 1, 1));
                    }
                }
                assert_eq!(active(&app), Some(Kind::About), "context {context}, route {route}");
                assert!(!app.menu_active && !app.menu_open);
                terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
                for expected in ["About VTCode", "VTCode ", env!("CARGO_PKG_VERSION"),
                    "Platform:", "Build:", "License:", "SSH protocol:",
                    "https://github.com/im-henri/vtcode", "Copy Info", "Close"] {
                    assert!(screen.contains(expected), "Missing {expected}: context {context}, route {route}");
                }
                assert_eq!(app.modal.focus, 1, "Enter should close About by default");
            }
        }
    }

    #[test]
    fn about_copy_button_and_shortcut_copy_the_displayed_info_without_editing() {
        let _clipboard = crate::clipboard::test_terminal_clipboard();
        let mut app = app();
        let original = app.editor().unwrap().rope.clone();
        let version = app.editor().unwrap().content_version;
        app.execute_palette_command("about");
        let info = app.about_info.clone().unwrap();
        render(&mut app, 100, 30);
        let copy = app.modal.buttons[0].0;
        click(&mut app, copy);
        assert_eq!(crate::clipboard::get_text().unwrap(), info);
        assert_eq!(active(&app), Some(Kind::About));
        crate::clipboard::set_text("another clipboard value").unwrap();
        crate::input_keyboard::handle_keyboard_event(
            &mut app, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(crate::clipboard::get_text().unwrap(), info);
        assert_eq!(app.editor().unwrap().rope, original);
        assert_eq!(app.editor().unwrap().content_version, version);
        assert!(app.modal.error.is_none());
    }

    #[test]
    fn about_blocks_editor_input_and_dismisses_using_standard_dialog_controls() {
        for action in 0..5 {
            let mut app = app();
            let original = app.editor().unwrap().rope.clone();
            app.execute_palette_command("about");
            render(&mut app, 100, 30);
            for key in [KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)] {
                crate::input_keyboard::handle_keyboard_event(&mut app, key);
                assert_eq!(active(&app), Some(Kind::About));
                assert_eq!(app.editor().unwrap().rope, original);
                assert!(!app.save_as_active);
            }
            match action {
                0 | 1 => {
                    crate::input_keyboard::handle_keyboard_event(&mut app,
                        KeyEvent::new(if action == 0 { KeyCode::Esc } else { KeyCode::Enter }, KeyModifiers::NONE));
                }
                2 => { let close = app.modal.close; click(&mut app, close); }
                3 => { let close = app.modal.buttons[1].0; click(&mut app, close); }
                _ => click(&mut app, Rect::new(0, 0, 1, 1)),
            }
            assert!(app.about_info.is_none());
            assert!(active(&app).is_none());
            assert_eq!(app.editor().unwrap().rope, original);
        }
    }

    #[test]
    fn multiline_dialog_messages_keep_hard_breaks_and_blank_lines_visible() {
        for newline in ["\n", "\r\n"] {
            let mut app = app();
            let message = ["The operation failed.", "", "Details:", "The final diagnostic line."].join(newline);
            app.show_save_error(message);
            let terminal = render(&mut app, 100, 30);
            let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
            assert!(screen.contains("The final diagnostic line."));
            assert_eq!(wrapped_height("one\n\nthree", 70), 3);
        }
    }

    #[test]
    fn oversized_multiline_errors_fit_the_viewport_and_leave_dismiss_accessible() {
        let mut app = app();
        app.show_save_error("diagnostic\n".repeat(u16::MAX as usize + 1));
        render(&mut app, 80, 24);
        assert_eq!(app.modal.buttons.len(), 1);
        let dismiss = app.modal.buttons[0].0;
        assert!(app.modal.area.contains((dismiss.x, dismiss.y).into()));
        click(&mut app, dismiss);
        assert!(active(&app).is_none());
    }

    fn enable(app: &mut App, kind: Kind) {
        match kind {
            Kind::About => app.show_about(),
            Kind::SaveError => app.show_save_error("This file changed on disk. Save cancelled."),
            Kind::FormattingError => app.show_formatting_error("The language server could not format this document."),
            Kind::AgentSettings => app.open_agent_settings(),
            Kind::LspSettings => app.open_lsp_settings(),
            Kind::ScopeBubbleSettings => app.open_scope_bubble_settings(),
            Kind::ExclusionSettings => app.open_exclusion_settings("files_exclude", None, false),
            Kind::FileAssociations => app.open_file_associations(),
            Kind::FormatterSettings => app.open_formatter_settings(None),
            Kind::CloseAgent => app.chat.confirm_close_agent = Some(app.chat.active_agent),
            Kind::ThemeInstall => app.open_theme_install_prompt(),
            Kind::ThemeDelete => {
                let mut theme = crate::app_theme_loader::builtin_theme("dark").unwrap().clone();
                theme.config_key = "test-installed-theme".into();
                theme.name = "Installed Theme".into();
                theme.builtin = false;
                app.loaded_themes.push(theme);
                app.open_settings_dropdown("theme", &app.theme_name.clone());
                app.request_theme_removal(app.loaded_themes.len() - 1);
            }
            Kind::ThemeVsixLink => {
                app.open_theme_install_prompt();
                app.palette.query = "https://vscodethemes.com/e/akamud.vscode-theme-onedark/atom-one-dark".into();
                app.show_theme_vsix_link();
            }
            Kind::SaveAs => {
                app.save_as_active = true;
                app.save_as_query = app
                    .explorer
                    .root
                    .join("release-notes.md")
                    .display()
                    .to_string();
                app.save_as_cursor = app.save_as_query.len();
            }
            Kind::Rename => {
                app.rename.symbol_active = true;
                app.rename.symbol_old = "load_workspace".into();
                app.rename.symbol_new = "load_project".into();
                app.rename.symbol_cursor = app.rename.symbol_new.len();
            }
            Kind::RenameTerminal => {
                app.new_terminal_tab();
                app.begin_terminal_rename(app.active_terminal);
            }
            Kind::Goto => app.open_goto_line(),
            Kind::Close => app.confirm_close_active = true,
            Kind::Delete => {
                app.confirm_delete_active = true;
                app.confirm_delete_target = Some(
                    app.explorer
                        .root
                        .join("design/旧版/very-long-archived-project-name"),
                );
            }
            Kind::Move => {
                app.confirm_move_active = true;
                app.confirm_move_source = Some(app.explorer.root.join("design/components.rs"));
                app.confirm_move_destination = Some(app.explorer.root.join("src/components.rs"));
            }
            Kind::Discard => app.sc_discard_confirm_active = true,
            Kind::StageAllCommit => {
                app.sc_stage_all_commit_request = Some(crate::app_git::GitCommitRequest {
                    root: app.explorer.root.clone(),
                    message: "Improve Source Control commit flow".into(),
                });
            }
            Kind::Exit => {
                app.editor_mut().unwrap().modified = true;
                app.exit_confirm_active = true;
            }
            Kind::ExitBackup => {
                app.exit_confirm_active = true;
                app.exit_backup_warning = Some(crate::app_exit_backup::Warning {
                    files: vec![crate::app_exit_backup::File { name: "large.log".into(), bytes: 20 * 1024 * 1024, history_bytes: 0 }],
                    total_bytes: 20 * 1024 * 1024,
                });
            }
            Kind::ExitRecovery => app.start_exit_recovery(),
        }
    }

    fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                text(
                    f,
                    f.area(),
                    "  VT Code  ·  workspace.rs\n\n  fn main() {\n      load_workspace();\n  }",
                    Style::default()
                        .fg(app.colors.fg_text)
                        .bg(app.colors.bg_editor),
                );
                draw_active(f, app);
            })
            .unwrap();
        terminal
    }

    fn click(app: &mut App, area: Rect) {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    fn assert_button_focus(app: &App, terminal: &Terminal<TestBackend>, selected: usize) {
        let buffer = terminal.backend().buffer();
        for &(area, index) in &app.modal.buttons {
            let row: String = (area.x..area.right()).map(|x| buffer[(x, area.y)].symbol()).collect();
            if index == selected {
                assert!(row.starts_with('[') && row.ends_with(']'), "active button must have a visible outline: {row:?}");
                assert_eq!(buffer[(area.x, area.y)].bg, (if index == 0 {
                    app.colors.ui.primary_button_hover
                } else { app.colors.ui.secondary_button_hover }).over(app.colors.ui.bg_widget).unwrap());
                assert!(buffer[(area.x, area.y)].modifier.contains(Modifier::BOLD));
            } else {
                assert!(!row.contains('['), "only one button may look active: {row:?}");
                let disabled = app.modal.kind == Some(Kind::ThemeInstall)
                    && app.install_prompt_running() && index == 0;
                let control = if index == 0 && (!disabled || app.colors.ui.preserve_theme_colors) { app.colors.ui.primary_button }
                    else { app.colors.ui.secondary_button };
                let surface = app.colors.ui.bg_widget;
                let mut background = control.background.over(surface).unwrap_or(surface);
                if disabled && app.colors.ui.preserve_theme_colors && let ratatui::style::Color::Rgb(r, g, b) = background {
                    background = crate::theme_workbench::ColorLayer::rgba(r, g, b, 102).over(surface).unwrap();
                }
                assert_eq!(buffer[(area.x, area.y)].bg, background);
            }
        }
    }

    #[test]
    fn dialog_buttons_share_one_clear_mouse_and_keyboard_selection() {
        for (kind, unsaved) in [(Kind::Exit, false), (Kind::Exit, true), (Kind::Close, true), (Kind::Delete, false)] {
            let mut app = app();
            enable(&mut app, kind);
            app.editor_mut().unwrap().modified = unsaved;
            // Hover and primary colors may be indistinguishable in a theme.
            app.colors.ui.primary_button.background = app.colors.ui.secondary_button.background;
            app.colors.ui.primary_button_hover = app.colors.ui.secondary_button.background;
            let terminal = render(&mut app, 100, 30);
            assert_button_focus(&app, &terminal, 0);
            let count = labels(&app, kind).len();
            for key in [KeyCode::Right, KeyCode::Down, KeyCode::Left, KeyCode::Up, KeyCode::Tab, KeyCode::BackTab] {
                let previous = app.modal.focus;
                let backwards = matches!(key, KeyCode::Left | KeyCode::Up | KeyCode::BackTab);
                handle_key(&mut app, KeyEvent::new(key, KeyModifiers::NONE));
                assert_eq!(app.modal.focus, (previous + if backwards { count - 1 } else { 1 }) % count);
                let terminal = render(&mut app, 100, 30);
                assert_button_focus(&app, &terminal, app.modal.focus);
            }
            let cancel = app.modal.buttons.last().unwrap().0;
            let movement = MouseEvent { kind: MouseEventKind::Moved, column: cancel.x + 1, row: cancel.y, modifiers: KeyModifiers::NONE };
            crate::mouse::handle_mouse(&mut app, movement);
            assert_eq!(app.modal.focus, count - 1, "hover changes the Enter target");
            let terminal = render(&mut app, 100, 30);
            assert_button_focus(&app, &terminal, count - 1);
            handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
            // A stationary pointer or repeated same-position report must not
            // resurrect a second highlight after keyboard navigation.
            crate::mouse::handle_mouse(&mut app, movement);
            let terminal = render(&mut app, 100, 30);
            assert_eq!(app.modal.focus, count - 2);
            assert_button_focus(&app, &terminal, count - 2);
            crate::mouse::handle_mouse(&mut app, MouseEvent { column: movement.column + 1, ..movement });
            assert_eq!(app.modal.focus, count - 1);
            assert_eq!(handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Some(false));
            assert!(active(&app).is_none(), "Enter must activate the hovered Cancel");
        }
    }

    #[test]
    fn dialog_buttons_skip_disabled_actions_and_preserve_field_editing() {
        let mut app = app();
        enable(&mut app, Kind::ThemeInstall);
        app.palette.query = "theme.json".into();
        app.palette.cursor = app.palette.query.len();
        render(&mut app, 100, 30);
        let cursor = app.palette.cursor;
        handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.modal.focus, 0);
        assert_eq!(app.palette.cursor, cursor - 1);
        app.palette.theme_install_running = true;
        let terminal = render(&mut app, 100, 30);
        assert_eq!(app.modal.focus, 2);
        assert_button_focus(&app, &terminal, 1);
        for key in [KeyCode::Tab, KeyCode::Left, KeyCode::Down, KeyCode::BackTab] {
            handle_key(&mut app, KeyEvent::new(key, KeyModifiers::NONE));
            assert_eq!(app.modal.focus, 2);
        }
        let disabled = app.modal.buttons[0].0;
        click(&mut app, disabled);
        assert!(app.palette.theme_install_running);
        assert_eq!(app.modal.focus, 2);
    }

    #[test]
    fn vsix_link_action_is_only_shown_for_valid_theme_gallery_links() {
        let mut app = app();
        enable(&mut app, Kind::ThemeInstall);
        for source in [
            "",
            "theme.vsix",
            "C:\\themes\\theme.json",
            "https://vscodethemes.com/",
            "https://vscodethemes.com/e/missing-extension-id/dark",
            "https://vscodethemes.com.evil.example/e/example.theme/dark",
            "https://example.com/theme.vsix",
        ] {
            app.palette.query = source.into();
            assert_eq!(labels(&app, Kind::ThemeInstall), ["Install theme", "Close"]);
        }
        app.palette.query = "https://vscodethemes.com/e/example.theme/dark".into();
        assert_eq!(labels(&app, Kind::ThemeInstall), ["Install theme", "Get VSIX link", "Close"]);
        app.palette.install_extension_mode = true;
        assert_eq!(labels(&app, Kind::ThemeInstall), ["Choose version…", "Close"]);
    }

    #[test]
    fn vsix_link_is_accessible_before_during_and_after_failed_install() {
        let source = "https://vscodethemes.com/e/akamud.vscode-theme-onedark/atom-one-dark?language=py";
        let url = "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/akamud/vsextensions/vscode-theme-onedark/latest/vspackage";
        for phase in 0..3 {
            for mouse in [false, true] {
                let mut app = app();
                enable(&mut app, Kind::ThemeInstall);
                assert!(app.paste_into_focused_text_input(source));
                if phase > 0 {
                    app.palette.theme_install_running = true;
                }
                if phase == 2 {
                    app.finish_theme_install(Err("Proxy connection refused".into()));
                    assert!(app.palette.theme_install_error.as_deref().unwrap().contains("Get VSIX link"));
                }
                let error = app.palette.theme_install_error.clone();
                render(&mut app, 100, 30);
                if mouse {
                    let button = app.modal.buttons[1].0;
                    click(&mut app, button);
                } else {
                    // Input and Install are skipped while a download is running.
                    if phase != 1 {
                        for _ in 0..2 {
                            handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
                        }
                    }
                    assert_eq!(app.modal.focus, 2);
                    handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
                assert_eq!(active(&app), Some(Kind::ThemeVsixLink));
                assert_eq!(app.palette.theme_vsix_url.as_deref(), Some(url));
                assert_eq!(app.palette.theme_install_running, phase == 1);
                let terminal = render(&mut app, 100, 30);
                let body = app.modal.area.inner(ratatui::layout::Margin::new(3, 1));
                let buffer = terminal.backend().buffer();
                let rendered: String = (body.y..body.bottom())
                    .flat_map(|y| (body.x..body.right()).map(move |x| buffer[(x, y)].symbol()))
                    .flat_map(str::chars)
                    .filter(|ch| !ch.is_whitespace())
                    .collect();
                assert!(rendered.contains(url), "the entire download URL must be visible");
                assert!(app.text_input_regions.is_empty());
                assert!(app.paste_into_focused_text_input("do not replace the source"));
                assert_eq!(app.palette.query, source);
                assert_eq!(labels(&app, Kind::ThemeVsixLink), ["Copy link", "Open in browser", "Back"]);
                if mouse {
                    let back = app.modal.buttons.last().unwrap().0;
                    click(&mut app, back);
                } else {
                    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                }
                assert_eq!(active(&app), Some(Kind::ThemeInstall));
                assert!(app.palette.theme_vsix_url.is_none());
                assert_eq!(app.palette.query, source);
                assert_eq!(app.palette.theme_install_error, error);
                assert_eq!(app.palette.theme_install_running, phase == 1);
            }
        }
    }

    #[test]
    fn polished_dialog_layouts_fit_and_expose_all_actions() {
        for kind in [
            Kind::About,
            Kind::SaveError,
            Kind::CloseAgent,
            Kind::ThemeInstall,
            Kind::ThemeVsixLink,
            Kind::ThemeDelete,
            Kind::SaveAs,
            Kind::Rename,
            Kind::RenameTerminal,
            Kind::Goto,
            Kind::Close,
            Kind::Delete,
            Kind::Move,
            Kind::Discard,
            Kind::StageAllCommit,
            Kind::Exit,
        ] {
            let mut app = app();
            enable(&mut app, kind);
            for (width, height) in [(100, 30), (80, 24), (40, 20), (30, 12), (16, 8), (3, 3)] {
                let terminal = render(&mut app, width, height);
                let rendered: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
                assert!(!rendered.contains("Switch control"));
                assert!(!rendered.contains("Enter  Confirm"));
                assert!(!rendered.contains("Tab Move"));
                let viewport = Rect::new(0, 0, width, height);
                assert_eq!(app.modal.area.intersection(viewport), app.modal.area);
                for (area, _) in &app.modal.buttons {
                    assert_eq!(
                        area.intersection(app.modal.area),
                        *area,
                        "{kind:?} {width}x{height}: {area:?}"
                    );
                }
                if width >= 30 && height >= 12 {
                    assert_eq!(
                        app.modal.buttons.len(),
                        labels(&app, kind).len(),
                        "{kind:?} {width}x{height}"
                    );
                }
                if width == 100 || width == 40 {
                    if let Ok(root) = std::env::var("VTCODE_MODAL_SNAPSHOTS") {
                        std::fs::create_dir_all(&root).unwrap();
                        let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({
                            "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
                            "bold": cell.modifier.contains(Modifier::BOLD), "dim": cell.modifier.contains(Modifier::DIM),
                        })).collect();
                        std::fs::write(PathBuf::from(root).join(format!("{kind:?}-{width}x{height}.json")), serde_json::to_vec(&serde_json::json!({"width": width, "height": height, "cells": cells})).unwrap()).unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn polished_goto_mouse_dismiss_restores_preview_and_blocks_editor_input() {
        let mut app = app();
        enable(&mut app, Kind::Goto);
        app.goto_line_query = "3".into();
        app.preview_goto_line();
        assert_eq!(app.editor().unwrap().cursor_line, 2);
        render(&mut app, 100, 30);
        click(&mut app, Rect::new(0, 0, 1, 1));
        assert!(!app.goto_line_active);
        assert_eq!(app.editor().unwrap().cursor_line, 0);
        enable(&mut app, Kind::Goto);
        render(&mut app, 100, 30);
        let before = app.editor().unwrap().rope.to_string();
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        );
        assert_eq!(app.modal.focus, 1);
        assert!(app.paste_into_focused_text_input("do not edit the file"));
        assert_eq!(app.editor().unwrap().rope.to_string(), before);
        assert!(app.goto_line_query.is_empty());
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        assert_eq!(app.modal.focus, 0);
    }

    #[test]
    fn polished_dialog_close_and_cancel_hit_regions_are_real() {
        for kind in [
            Kind::ThemeInstall,
            Kind::ThemeDelete,
            Kind::SaveAs,
            Kind::Rename,
            Kind::RenameTerminal,
            Kind::Goto,
            Kind::Close,
            Kind::Delete,
            Kind::Move,
            Kind::Discard,
            Kind::StageAllCommit,
            Kind::Exit,
        ] {
            for close in [false, true] {
                let mut app = app();
                enable(&mut app, kind);
                render(&mut app, 80, 24);
                let target = if close {
                    app.modal.close
                } else {
                    app.modal.buttons.last().unwrap().0
                };
                click(&mut app, target);
                assert!(active(&app).is_none(), "{kind:?} close={close}");
            }
        }
    }

    #[test]
    fn polished_save_as_validates_and_requires_explicit_replacement() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("existing.txt");
        std::fs::write(&target, "original").unwrap();
        let mut app = app();
        app.explorer.root = root.path().to_path_buf();
        enable(&mut app, Kind::SaveAs);
        app.save_as_query.clear();
        app.execute_save_as();
        assert!(app.save_as_active && app.modal.error.is_some());
        assert!(app.editor().unwrap().file_path.is_none());
        app.save_as_query = "missing/file.txt".into();
        app.execute_save_as();
        assert!(app.save_as_active && app.modal.error.is_some());
        app.save_as_query = target.display().to_string();
        app.execute_save_as();
        assert!(app.save_as_active);
        assert_eq!(app.modal.overwrite, Some(target.clone()));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert!(app.editor().unwrap().file_path.is_none());
        app.execute_save_as();
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.pending_file_saves.is_empty()
        });
        assert!(app.save_error.is_none());
        assert!(std::fs::read_to_string(&target).unwrap().starts_with("one\n"));
        assert!(!app.save_as_active);
        assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&target));
    }

    #[test]
    fn polished_save_as_browse_preserves_name_and_workspace_on_select_and_cancel() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("output");
        std::fs::create_dir(&destination).unwrap();
        let mut app = app();
        app.explorer.root = root.path().to_path_buf();
        enable(&mut app, Kind::SaveAs);
        render(&mut app, 100, 30);
        activate(&mut app, Kind::SaveAs, 1);
        assert!(app.change_dir_active && !app.save_as_active);
        app.navigate_folder_browser(destination.clone());
        app.wait_for_background(Duration::from_secs(10), |a| !a.folder_browser_loading);
        app.confirm_folder_browser();
        assert!(app.save_as_active && !app.change_dir_active);
        assert_eq!(app.explorer.root, root.path());
        assert_eq!(
            PathBuf::from(&app.save_as_query),
            destination.join("release-notes.md")
        );
        let selected = app.save_as_query.clone();
        activate(&mut app, Kind::SaveAs, 1);
        app.folder_browser_action(crate::folder_browser::Action::Close);
        assert!(app.save_as_active && !app.change_dir_active);
        assert_eq!(app.save_as_query, selected);
    }

    #[test]
    fn polished_theme_install_form_validates_and_blocks_edits_while_busy() {
        let mut app = app();
        enable(&mut app, Kind::ThemeInstall);
        render(&mut app, 80, 24);
        let install = app.modal.buttons[0].0;
        click(&mut app, install);
        assert!(app.palette.theme_install_error.is_some());
        assert!(!app.palette.theme_install_running);
        app.modal.focus = 0;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(app.palette.query, "x");
        assert!(app.palette.theme_install_error.is_none());
        app.palette.theme_install_running = true;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(app.paste_into_focused_text_input("do not replace the active source"));
        assert_eq!(app.palette.query, "x");
        render(&mut app, 80, 24);
        let close = app.modal.close;
        click(&mut app, close);
        assert!(!app.theme_install_prompt_visible());
    }

    #[test]
    fn polished_save_as_editing_clears_stale_replacement_approval() {
        let mut app = app();
        enable(&mut app, Kind::SaveAs);
        sync(&mut app, Kind::SaveAs);
        app.modal.overwrite = Some(PathBuf::from(&app.save_as_query));
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(app.modal.overwrite.is_none());
        app.modal.overwrite = Some(PathBuf::from(&app.save_as_query));
        app.paste_into_focused_text_input("different");
        assert!(app.modal.overwrite.is_none());
    }
}
