//! Ctrl-hover/click navigation uses the retained terminal grid. Path resolution
//! stays lexical; remote classification and reads happen only on click.
use crate::{app::{ActivePane, App}, app_terminal_selection::TerminalSurface, terminal::links::{Cache, Location}};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, style::Modifier, text::Line};
use std::path::PathBuf;

#[derive(Clone, Copy)]
struct Hover { surface: TerminalSurface, link: usize }

#[derive(Default)]
pub(crate) struct Controller {
    cache: Cache,
    hover: Option<Hover>,
    captured: bool,
}

impl Controller {
    pub(crate) fn begin_frame(&mut self) { self.hover = None; }
}

fn available(app: &App) -> bool {
    app.terminal_has_focus && app.lsp_link_hover.modifiers == KeyModifiers::CONTROL
        && !app.context_menu_active && !app.tab_context_active && !app.menu_active
        && !app.explorer_context_active && !app.workspaces.popup.open
        && crate::ui_modal::active(app).is_none() && crate::ui_quick_picker::active(app).is_none()
        && app.terminal_selection_drag.is_none() && app.terminal_mouse_capture.is_none()
        && !app.terminal_find.contains((app.mouse_col, app.mouse_row))
}

fn hit(app: &mut App, surface: TerminalSurface, column: u16, row: u16) -> Option<usize> {
    if !surface.area.contains((column, row).into()) { return None; }
    let terminal = app.terminals.get(surface.index)?.terminal_pty.as_ref()?;
    app.terminal_links.cache.hit(&terminal.grid, usize::from(row - surface.area.y), usize::from(column - surface.area.x))
}

fn local_directory(value: &str) -> Option<PathBuf> {
    let uri = url::Url::parse(value).ok()?;
    #[cfg(windows)]
    let uri = {
        let mut uri = uri;
        // Git Bash and Cygwin report POSIX drive spellings in OSC 7.
        let path = uri.path().strip_prefix("/cygdrive").unwrap_or(uri.path());
        let bytes = path.as_bytes();
        if bytes.first() == Some(&b'/') && bytes.get(1).is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(2) == Some(&b'/')
        {
            let mapped = format!("/{}:{}", bytes[1] as char, &path[2..]);
            uri.set_host(None).ok()?;
            uri.set_path(&mapped);
        }
        uri
    };
    uri.to_file_path().ok()
}

pub(crate) fn resolve_path(app: &App, index: usize, value: &str) -> Result<PathBuf, String> {
    let terminal = app.terminals.get(index).ok_or("Terminal is no longer available")?;
    let cwd_uri = terminal.terminal_pty.as_ref().and_then(|pty| pty.grid.cwd_uri.as_deref());
    if let Some(remote) = &app.remote_workspace {
        let cwd = cwd_uri.and_then(|uri| remote.target_key(uri).ok())
            .unwrap_or_else(|| terminal.terminal_cwd.clone());
        if !remote.client.hello().root.starts_with('/') {
            let value = value.replace('\\', "/");
            if value.as_bytes().get(1) == Some(&b':') && value.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
                return remote.target_key(&format!("/{value}")).map_err(|error| error.to_string());
            }
            return remote.resolve_from_key(&cwd, &value).map_err(|error| error.to_string());
        }
        return remote.resolve_from_key(&cwd, value).map_err(|error| error.to_string());
    }
    let cwd = cwd_uri.and_then(local_directory)
        .unwrap_or_else(|| terminal.terminal_cwd.clone());
    let path = PathBuf::from(value);
    Ok(if path.is_absolute() { path } else { cwd.join(path) })
}

fn open(app: &mut App, surface: TerminalSurface, location: Location) {
    let path = match resolve_path(app, surface.index, &location.path) {
        Ok(path) => path,
        Err(error) => { app.notify_warn(error); return; }
    };
    app.focus_terminal_surface(surface);
    if app.remote_workspace.is_some() {
        app.open_remote_location_at(path, location.line, location.column, crate::app_remote_navigation::Source::Terminal);
    } else {
        app.push_nav_history();
        app.open_file(&path);
        app.terminal_panel_focused = false;
        app.active_pane = ActivePane::Editor;
        if let Some(line) = location.line && let Some(editor) = app.editor_mut()
            && editor.file_path.as_deref().is_some_and(|other| App::same_open_path(other, &path))
        {
            editor.set_centered_cursor(line as usize, location.column as usize);
        }
    }
}

pub(crate) fn mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if app.terminal_links.captured {
        match mouse.kind {
            MouseEventKind::Up(MouseButton::Left) => { app.terminal_links.captured = false; return true; }
            MouseEventKind::Drag(MouseButton::Left) => return true,
            MouseEventKind::Down(_) => app.terminal_links.captured = false,
            _ => {}
        }
    }
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || mouse.modifiers != KeyModifiers::CONTROL || !available(app) { return false; }
    let Some(surface) = app.terminal_surfaces.iter().rev().copied()
        .find(|surface| surface.area.contains((mouse.column, mouse.row).into())) else { return false; };
    let Some(index) = hit(app, surface, mouse.column, mouse.row) else { return false; };
    let location = app.terminal_links.cache.links[index].location.clone();
    open(app, surface, location);
    app.terminal_links.captured = true;
    app.terminal_links.hover = None;
    true
}

/// Patch only this frame's cells, leaving the terminal's damage cache intact.
pub(crate) fn draw(f: &mut Frame, app: &mut App, surface: TerminalSurface) {
    if !available(app) { return; }
    let Some(index) = hit(app, surface, app.mouse_col, app.mouse_row) else { return; };
    let terminal = app.terminals[surface.index].terminal_pty.as_ref().unwrap();
    let viewport = terminal.grid.scrollback.len().saturating_sub(terminal.grid.scroll_view);
    let cache = &app.terminal_links.cache;
    let link = &cache.links[index];
    for visual in 0..usize::from(surface.area.height) {
        let absolute = viewport + visual;
        let Some(relative) = absolute.checked_sub(cache.first) else { continue; };
        if relative < link.start.row || relative > link.end.row { continue; }
        let start = if relative == link.start.row { link.start.column } else { 0 };
        let end = if relative == link.end.row { link.end.column } else { usize::from(surface.area.width) };
        for column in start..end.min(usize::from(surface.area.width)) {
            if let Some(cell) = f.buffer_mut().cell_mut((surface.area.x + column as u16, surface.area.y + visual as u16)) {
                cell.modifier.insert(Modifier::UNDERLINED);
            }
        }
    }
    app.terminal_links.hover = Some(Hover { surface, link: index });
}

pub(crate) fn draw_tooltip(f: &mut Frame, app: &App) {
    if !available(app) { return; }
    let Some(hover) = app.terminal_links.hover else { return; };
    if !app.terminal_surfaces.contains(&hover.surface) { return; }
    let location = &app.terminal_links.cache.links[hover.link].location;
    let hint = location.line.map_or_else(|| "Ctrl+click to open file".into(), |line| {
        format!("Ctrl+click to open file · line {}, column {}", u64::from(line) + 1, u64::from(location.column) + 1)
    });
    crate::ui_widgets::Tooltip::hover(vec![Line::raw(hint)], &app.colors)
        .render(f, app.mouse_col, app.mouse_row);
}

#[cfg(test)]
#[path = "app_terminal_links_tests.rs"]
mod tests;
