//! A directory opened by Ex is a navigable view in an editor pane.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Paragraph};

use super::paths::{self, Entry};
use crate::app::{ActivePane, App, Tab, TabDropZone, TabKind};
use crate::background::BackgroundResult;

#[derive(Clone)]
pub(crate) struct Directory {
    pub(crate) path: PathBuf,
    label: String,
    entries: Arc<[Entry]>,
    selected: usize,
    scroll: usize,
    list_area: Rect,
    request: Option<u64>,
    error: Option<String>,
}

pub(crate) struct Loaded {
    pub(crate) request: u64,
    pub(crate) entries: std::io::Result<Vec<Entry>>,
}

impl Directory {
    fn select(&mut self, index: usize) {
        self.selected = index.min(self.entries.len().saturating_sub(1));
        let height = usize::from(self.list_area.height).max(1);
        self.scroll = self
            .scroll
            .clamp(
                self.selected.saturating_add(1).saturating_sub(height),
                self.selected,
            )
            .min(self.entries.len().saturating_sub(height));
    }
}

pub(crate) fn current(app: &App) -> Option<&Directory> {
    app.tabs.get(app.active_tab)?.directory.as_deref()
}

fn current_mut(app: &mut App) -> Option<&mut Directory> {
    app.tabs.get_mut(app.active_tab)?.directory.as_deref_mut()
}

pub(crate) fn open(app: &mut App, path: PathBuf, split: Option<TabDropZone>) {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    app.cancel_remote_location();
    app.record_navigation();
    app.auto_save_on_focus_change();
    let request = NEXT.fetch_add(1, Ordering::Relaxed);
    let label = paths::label(app, &path);
    let name = path.file_name().map_or_else(
        || label.clone(),
        |name| {
            crate::remote::workspace::display_name(&path).unwrap_or_else(|| {
                crate::ui_workspaces::escaped(&name.to_string_lossy()).into_owned()
            })
        },
    );
    let directory = Box::new(Directory {
        path: path.clone(),
        label,
        entries: Arc::default(),
        selected: 0,
        scroll: 0,
        list_area: Rect::default(),
        request: Some(request),
        error: None,
    });
    if split.is_none() && current(app).is_some() {
        let tab = &mut app.tabs[app.active_tab];
        tab.editor.suggested_file_name = Some(name);
        tab.directory = Some(directory);
    } else {
        let mut editor = app.new_editor();
        editor.read_only = true;
        editor.suggested_file_name = Some(name);
        let tab = Tab {
            kind: TabKind::Directory,
            directory: Some(directory),
            ..Tab::new_file(editor)
        };
        if let Some(zone) = split {
            app.open_tab_in_split(tab, zone);
        } else {
            app.clear_transient_editor_state();
            app.active_tab = app.insert_tab_after_active(tab);
            app.touch_tab_mru(app.active_tab);
        }
    }
    app.active_pane = ActivePane::Editor;
    app.terminal_panel_focused = false;
    app.sync_diff_tab();
    app.ensure_active_tab_visible();
    app.record_navigation();

    let remote = app
        .remote_workspace
        .clone()
        .filter(|_| crate::remote::workspace::is_view_key(&path));
    let sender = app.background.sender.clone();
    let read_path = path.clone();
    app.background
        .read_for_file("vim-directory", &read_path, true, move || {
            let entries = paths::entries(&path, remote.as_ref()).map(|mut entries| {
                entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
                if let Some(parent) = paths::parent(&path, remote.as_ref()) {
                    entries.insert(
                        0,
                        Entry {
                            path: parent,
                            name: "..".into(),
                            is_dir: true,
                        },
                    );
                }
                entries
            });
            let _ = sender.send(BackgroundResult::VimDirectoryLoaded(Loaded {
                request,
                entries,
            }));
        });
}

pub(crate) fn apply(app: &mut App, loaded: Loaded) -> bool {
    let result = loaded
        .entries
        .map(Arc::<[Entry]>::from)
        .map_err(|error| error.to_string());
    let mut changed = false;
    for tab in app
        .tabs
        .iter_mut()
        .chain(app.groups.iter_mut().flat_map(|group| &mut group.tabs))
    {
        if let Some(directory) = tab.directory.as_mut()
            && directory.request == Some(loaded.request)
        {
            directory.request = None;
            match &result {
                Ok(entries) => {
                    directory.entries = entries.clone();
                    directory.select(usize::from(entries.len() > 1 && entries[0].name == ".."));
                }
                Err(error) => directory.error = Some(error.clone()),
            }
            changed = true;
        }
    }
    changed
}

fn activate(app: &mut App) {
    let Some(entry) = current(app).and_then(|dir| dir.entries.get(dir.selected)) else {
        return;
    };
    let path = entry.path.clone();
    if entry.is_dir {
        open(app, path, None);
    } else {
        app.push_nav_history();
        app.open_file_from_vim(&path, None, None);
    }
}

pub(crate) fn refresh(app: &mut App) {
    if let Some(dir) = current(app) {
        open(app, dir.path.clone(), None);
    }
}

pub(crate) fn key(app: &mut App, event: KeyEvent) -> bool {
    if app.active_pane != ActivePane::Editor || current(app).is_none() {
        return false;
    }
    if event.modifiers.is_empty() {
        let dir = current_mut(app).unwrap();
        let page = usize::from(dir.list_area.height).max(1);
        let index = match event.code {
            KeyCode::Down => dir.selected.saturating_add(1),
            KeyCode::Up => dir.selected.saturating_sub(1),
            KeyCode::PageDown => dir.selected.saturating_add(page),
            KeyCode::PageUp => dir.selected.saturating_sub(page),
            KeyCode::Home => 0,
            KeyCode::End => dir.entries.len().saturating_sub(1),
            KeyCode::Backspace | KeyCode::Left => {
                let path = dir.path.clone();
                if let Some(parent) = paths::parent(&path, app.remote_workspace.as_ref()) {
                    open(app, parent, None);
                }
                return true;
            }
            KeyCode::Enter | KeyCode::Right => {
                activate(app);
                return true;
            }
            _ => return true,
        };
        dir.select(index);
    }
    // A directory view never passes text-editing keys to the editor.
    true
}

pub(crate) fn mouse(app: &mut App, event: MouseEvent) -> bool {
    let position = Position::new(event.column, event.row);
    let hit = |tab: &Tab| {
        tab.directory
            .as_ref()
            .is_some_and(|dir| dir.list_area.contains(position))
    };
    let group = if app.tabs.get(app.active_tab).is_some_and(hit) {
        app.active_group
    } else {
        let Some(group) = app.groups.iter().enumerate().find_map(|(i, group)| {
            let tab = group.tabs.get(group.active_tab)?;
            (hit(tab)
                && app.visible_tab_group(tab.editor.navigation_id, TabKind::Directory) == Some(i))
            .then_some(i)
        }) else {
            return false;
        };
        group
    };
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.switch_group(group);
            app.active_pane = ActivePane::Editor;
            app.terminal_panel_focused = false;
            let dir = current_mut(app).unwrap();
            let index = dir.scroll + usize::from(event.row - dir.list_area.y);
            if index < dir.entries.len() {
                dir.select(index);
                activate(app);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let dir = if group == app.active_group {
                current_mut(app).unwrap()
            } else {
                let group = &mut app.groups[group];
                group.tabs[group.active_tab].directory.as_mut().unwrap()
            };
            let index = if event.kind == MouseEventKind::ScrollUp {
                dir.selected.saturating_sub(3)
            } else {
                dir.selected.saturating_add(3)
            };
            dir.select(index);
        }
        _ => {}
    }
    true
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    app.code_area = None;
    app.gutter_area = None;
    app.minimap_area = None;
    app.scrollbar_area = None;
    app.h_scrollbar_area = None;
    let colors = &app.colors;
    let Some(dir) = app
        .tabs
        .get_mut(app.active_tab)
        .and_then(|tab| tab.directory.as_deref_mut())
    else {
        return;
    };
    let bg = colors.bg_editor;
    f.render_widget(Block::default().style(Style::default().bg(bg)), area);
    if area.is_empty() {
        dir.list_area = Rect::default();
        return;
    }
    let header = Rect::new(area.x, area.y, area.width, 1);
    f.render_widget(
        Paragraph::new(dir.label.as_str()).style(Style::default().fg(colors.fg_dim).bg(bg)),
        header,
    );
    dir.list_area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    if dir.list_area.is_empty() {
        return;
    }
    if dir.request.is_some() || dir.error.is_some() || dir.entries.is_empty() {
        let text = if dir.request.is_some() {
            "Loading directory…".into()
        } else if let Some(error) = &dir.error {
            format!(
                "Cannot read directory: {}",
                crate::ui_workspaces::escaped(error)
            )
        } else {
            "Empty directory".into()
        };
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(colors.fg_dim)),
            dir.list_area,
        );
        return;
    }
    dir.select(dir.selected);
    for (i, entry) in dir
        .entries
        .iter()
        .skip(dir.scroll)
        .take(dir.list_area.height as usize)
        .enumerate()
    {
        let name = crate::ui_workspaces::escaped(&entry.name);
        let text = if entry.is_dir {
            format!("{name}/").into()
        } else {
            name
        };
        let mut style = Style::default().fg(colors.fg_text).bg(bg);
        if entry.is_dir {
            style = style.add_modifier(Modifier::BOLD);
        }
        if dir.scroll + i == dir.selected {
            style = style.fg(colors.fg_selected).bg(colors.bg_selected);
        }
        let row = Rect::new(
            dir.list_area.x,
            dir.list_area.y + i as u16,
            dir.list_area.width,
            1,
        );
        f.render_widget(Paragraph::new(text).style(style), row);
    }
}
