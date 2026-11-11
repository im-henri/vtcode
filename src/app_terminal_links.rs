//! Ctrl-hover/click navigation uses the retained terminal grid. Path resolution
//! stays lexical; remote classification and reads happen only on click.
use crate::{app::{ActivePane, App, NotifyLevel}, app_terminal_selection::TerminalSurface, terminal::links::{Cache, Location}};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, style::Modifier, text::Line};
use std::{io, path::{Path, PathBuf}};

#[derive(Clone, Copy)]
struct Hover { surface: TerminalSurface, link: usize }

#[derive(Default)]
pub(crate) struct Controller {
    cache: Cache,
    hover: Option<Hover>,
    captured: bool,
    directory: Option<(u64, PathBuf, crate::app_remote_navigation::Origin)>,
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
    app.side_notice.remove("terminal-path-error");
    app.cancel_terminal_directory_reveal();
    let path = match resolve_path(app, surface.index, &location.path) {
        Ok(path) => path,
        Err(error) => { show_error(app, error); return; }
    };
    app.focus_terminal_surface(surface);
    app.open_terminal_location_at(path, location.line, location.column);
}

/// Local filesystem access shares the cancellable link worker used for SSH.
/// Canonical identities map directory aliases back into the current tree.
pub(crate) fn open_local_location(path: &Path, root: &Path, cancel: &crate::background::CancellationToken, source: crate::app_remote_navigation::Source)
    -> io::Result<crate::remote::workspace::Location>
{
    use crate::{app_remote_navigation::Source, remote::workspace::Location};
    let result = (|| {
        if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
        let metadata = std::fs::metadata(path)?;
        if metadata.is_dir() {
            if source == Source::PathDialog {
                drop(std::fs::read_dir(path)?);
                return Ok(Location::Directory(path.to_path_buf()));
            }
            if source == Source::FileCommand { return Err(io::Error::new(io::ErrorKind::IsADirectory, "Choose a file, not a folder")); }
            let canonical = path.canonicalize()?;
            let canonical_root = root.canonicalize()?;
            let relative = crate::native_paths::strip_prefix(&canonical, &canonical_root)
                .ok_or_else(|| io::Error::other("Folder is outside the current workspace"))?;
            // Opening a directory listing verifies access without walking it.
            drop(std::fs::read_dir(&canonical)?);
            return Ok(Location::Directory(root.join(relative)));
        }
        let (file, metadata) = crate::fs_ops::open_document_source(path, false)?;
        crate::editor::decode_text_source(file, metadata, path, Some(cancel)).map(Location::File)
    })();
    result.map_err(|error: io::Error| match error.kind() {
        io::ErrorKind::NotFound => io::Error::new(error.kind(), "Path does not exist"),
        io::ErrorKind::PermissionDenied => io::Error::new(error.kind(), "Permission denied for this path"),
        _ => error,
    })
}

pub(crate) fn show_error(app: &mut App, error: String) {
    app.notify_warn(error.clone());
    show_error_notice(app, error);
}

pub(crate) fn show_path_error(app: &mut App, path: &Path, error: String) {
    app.notify_warn(format!("Cannot open {}: {error}", app.display_path(path)));
    show_error_notice(app, error);
}

fn show_error_notice(app: &mut App, error: String) {
    let message = if error.contains("outside the current workspace") {
        "Folder is outside the current workspace".into()
    } else { error };
    app.side_notice.show("terminal-path-error", crate::ui_side_notice::Notice {
        title: "Cannot open terminal path",
        message,
        level: NotifyLevel::Warning,
        actions: &[crate::ui_side_notice::Action { label: "Details", command: "show_notifications" }],
    });
}

impl App {
    pub(crate) fn cancel_terminal_directory_reveal(&mut self) {
        if let Some((_, path, _)) = self.terminal_links.directory.take() {
            self.explorer.cancel_pending_reveal(&path);
        }
    }

    pub(crate) fn cancel_stale_terminal_directory_reveal(&mut self) {
        if self.terminal_links.directory.as_ref().is_some_and(|(generation, _, origin)|
            *generation != self.workspace_generation || !origin.matches(self))
        {
            self.cancel_terminal_directory_reveal();
        }
    }

    pub(crate) fn reveal_terminal_directory(&mut self, path: &Path) {
        if crate::native_paths::strip_prefix(path, &self.explorer.root).is_none()
            || self.remote_workspace.as_ref().is_some_and(|workspace|
                workspace.remote_path(path).map_or(true, |path| path.is_absolute()))
        {
            show_path_error(self, path, "Folder is outside the current workspace".into());
            return;
        }
        let filtering = self.explorer.find.filtering();
        if filtering {
            self.explorer.find.filter = false;
            self.explorer.prepare_find();
        }
        self.explorer.find.select_first = false;
        self.terminal_links.directory = Some((self.workspace_generation, path.to_owned(),
            crate::app_remote_navigation::Origin::capture(self, crate::app_remote_navigation::Source::Terminal)));
        let refresh = if path == self.explorer.root {
            self.explorer.selected = 0;
            self.explorer.scroll_offset = 0;
            filtering
        } else {
            self.explorer.reveal_directory_deferred(path) || filtering
        };
        if refresh { self.refresh_explorer_tree_async(); }
        else { self.finish_terminal_directory_reveal(None); }
    }

    pub(crate) fn finish_terminal_directory_reveal(&mut self, error: Option<&str>) {
        let Some((generation, path, origin)) = self.terminal_links.directory.take() else { return; };
        if generation != self.workspace_generation || !origin.matches(self) { return; }
        if let Some(error) = error {
            show_path_error(self, &path, error.to_owned());
        } else if path != self.explorer.root && self.explorer.find_path(&path).is_none() {
            show_path_error(self, &path, "Folder is excluded, inaccessible or removed".into());
        } else {
            self.show_explorer = true;
            self.sidebar_view = crate::app::SidebarView::Explorer;
            self.terminal_panel_focused = false;
            self.active_pane = ActivePane::Explorer;
            self.clear_explorer_selection();
            self.set_explorer_single_selection(self.explorer.selected);
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
    let hint = location.line.map_or_else(|| "Ctrl+click to open file or reveal folder".into(), |line| {
        format!("Ctrl+click to open file · line {}, column {}", u64::from(line) + 1, u64::from(location.column) + 1)
    });
    crate::ui_widgets::Tooltip::hover(vec![Line::raw(hint)], &app.colors)
        .render(f, app.mouse_col, app.mouse_row);
}

#[cfg(test)]
#[path = "app_terminal_links_tests.rs"]
mod tests;
