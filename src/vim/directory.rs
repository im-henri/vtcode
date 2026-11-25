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
    /// `/` or `?` being typed: the query, its direction and where it began.
    search: Option<(String, bool, usize)>,
    /// The last search, for `n` and `N`.
    last_search: Option<(String, bool)>,
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
        search: None,
        last_search: current(app).and_then(|d| d.last_search.clone()),
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

/// The next entry from `from` (not itself) whose name contains `query`
/// (smartcase), wrapping around.
fn find_entry(
    dir: &Directory,
    query: &str,
    forward: bool,
    from: usize,
    include: bool,
) -> Option<usize> {
    let n = dir.entries.len();
    if n == 0 || query.is_empty() {
        return None;
    }
    let smart = query.chars().any(char::is_uppercase);
    let q = if smart {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let hit = |i: usize| {
        let name = &dir.entries[i].name;
        name != ".."
            && if smart {
                name.contains(&q)
            } else {
                name.to_lowercase().contains(&q)
            }
    };
    (0..n)
        .map(|k| {
            let k = if include { k } else { k + 1 };
            if forward {
                (from + k) % n
            } else {
                (from + n * 2 - k % n) % n
            }
        })
        .find(|&i| hit(i))
}

pub(crate) fn searching(app: &App) -> bool {
    current(app).is_some_and(|d| d.search.is_some())
}

/// `/` (or `?`): start typing a name to jump to.
pub(crate) fn start_search(app: &mut App, forward: bool) {
    if let Some(dir) = current_mut(app) {
        dir.search = Some((String::new(), forward, dir.selected));
    }
}

/// A key while typing the search: the selection follows the match.
pub(crate) fn search_key(app: &mut App, key: super::Key) {
    use super::keys::K;
    let Some(dir) = current_mut(app) else {
        return;
    };
    let Some((mut q, forward, origin)) = dir.search.take() else {
        return;
    };
    if key.is_escape() || key.is_ctrl('c') {
        dir.select(origin);
        return;
    }
    if key.k == K::Enter && !key.ctrl {
        if !q.is_empty() {
            dir.last_search = Some((q, forward));
        }
        return;
    }
    match key.k {
        K::Backspace => {
            if q.pop().is_none() {
                dir.select(origin);
                return;
            }
        }
        K::Char('u') if key.ctrl => q.clear(),
        K::Char(c) if !key.ctrl && !key.alt => q.push(c),
        _ => {}
    }
    match find_entry(dir, &q, forward, origin, false) {
        Some(i) => dir.select(i),
        None => dir.select(origin),
    }
    dir.search = Some((q, forward, origin));
}

/// `n` (`reverse` false) and `N`: the next match of the last search.
pub(crate) fn search_next(app: &mut App, reverse: bool) -> Result<(), String> {
    let Some(dir) = current_mut(app) else {
        return Ok(());
    };
    let Some((q, forward)) = dir.last_search.clone() else {
        return Err("E35: No previous regular expression".into());
    };
    let dir_forward = forward != reverse;
    match find_entry(dir, &q, dir_forward, dir.selected, false) {
        Some(i) => {
            dir.select(i);
            Ok(())
        }
        None => Err(format!("E486: Pattern not found: {q}")),
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
    use ratatui::text::{Line, Span};

    app.code_area = None;
    app.gutter_area = None;
    app.minimap_area = None;
    app.scrollbar_area = None;
    app.h_scrollbar_area = None;
    let colors = &app.colors;
    let associations = &app.language_associations;
    let Some(dir) = app
        .tabs
        .get_mut(app.active_tab)
        .and_then(|tab| tab.directory.as_deref_mut())
    else {
        return;
    };
    let bg = colors.bg_editor;
    let ui = &colors.ui;
    let dim = ui.readable_foreground(colors.fg_dim, bg, 3.0);
    let folder = ui.readable_foreground(colors.fg_folder_icon, bg, 3.0);
    f.render_widget(Block::default().style(Style::default().bg(bg)), area);
    if area.is_empty() {
        dir.list_area = Rect::default();
        return;
    }

    // Header: the parent path quiet, the folder itself bold, counts on the right.
    let label = dir.label.trim_end_matches(['/', '\\']);
    let (head, tail) = match label.rfind(['/', '\\']) {
        Some(i) => (&label[..=i], &label[i + 1..]),
        None => ("", label),
    };
    let folders = dir
        .entries
        .iter()
        .filter(|e| e.is_dir && e.name != "..")
        .count();
    let files = dir.entries.iter().filter(|e| !e.is_dir).count();
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let counts = if dir.request.is_some() {
        String::new()
    } else {
        format!(
            "{} · {} ",
            plural(folders, "folder", "folders"),
            plural(files, "file", "files")
        )
    };
    let header = Rect::new(area.x, area.y, area.width, 1);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ▾ ", Style::default().fg(folder)),
            Span::styled(
                crate::ui_workspaces::escaped(head).into_owned(),
                Style::default().fg(dim),
            ),
            Span::styled(
                crate::ui_workspaces::escaped(tail).into_owned(),
                Style::default()
                    .fg(colors.fg_text)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(bg)),
        header,
    );
    let counts_w = counts.chars().count() as u16;
    if counts_w > 0 && counts_w + 12 < area.width {
        f.render_widget(
            Paragraph::new(Span::styled(counts.as_str(), Style::default().fg(dim))),
            Rect::new(area.right() - counts_w, area.y, counts_w, 1),
        );
    }
    if area.height < 2 {
        dir.list_area = Rect::default();
        return;
    }
    let rule = Rect::new(area.x, area.y + 1, area.width, 1);
    f.render_widget(
        Paragraph::new("─".repeat(area.width as usize))
            .style(Style::default().fg(colors.border_color).bg(bg)),
        rule,
    );
    // A key hint along the bottom when there is room for it.
    let hint_rows = u16::from(area.height >= 8);
    dir.list_area = Rect::new(area.x, area.y + 2, area.width, area.height - 2 - hint_rows);
    if dir.list_area.is_empty() {
        return;
    }
    if dir.request.is_some() || dir.error.is_some() || dir.entries.is_empty() {
        let (text, color) = if dir.request.is_some() {
            ("Loading directory…".to_string(), dim)
        } else if let Some(error) = &dir.error {
            (
                format!(
                    "Cannot read directory: {}",
                    crate::ui_workspaces::escaped(error)
                ),
                colors.fg_error,
            )
        } else {
            ("Empty directory".into(), dim)
        };
        f.render_widget(
            Paragraph::new(format!("   {text}")).style(Style::default().fg(color).bg(bg)),
            dir.list_area,
        );
        return;
    }
    dir.select(dir.selected);
    let width = dir.list_area.width as usize;
    for (i, entry) in dir
        .entries
        .iter()
        .skip(dir.scroll)
        .take(dir.list_area.height as usize)
        .enumerate()
    {
        let selected = dir.scroll + i == dir.selected;
        let row_bg = if selected { colors.bg_selected } else { bg };
        let text_fg = if selected {
            colors.fg_selected
        } else {
            colors.fg_text
        };
        let quiet = ui.readable_foreground(colors.fg_dim, row_bg, 3.0);
        let parent = entry.name == "..";
        let hidden = entry.name.starts_with('.') && !parent;
        let name = crate::ui_workspaces::escaped(&entry.name).into_owned();
        let mut spans = vec![Span::raw(" ")];
        if parent {
            spans.push(Span::styled("↰  ", Style::default().fg(quiet)));
            spans.push(Span::styled("..", Style::default().fg(quiet)));
            spans.push(Span::styled("  parent folder", Style::default().fg(quiet)));
        } else if entry.is_dir {
            let icon = ui.readable_foreground(colors.fg_folder_icon, row_bg, 3.0);
            spans.push(Span::styled("▸  ", Style::default().fg(icon)));
            let fg = if hidden && !selected { quiet } else { text_fg };
            spans.push(Span::styled(
                name,
                Style::default().fg(fg).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled("/", Style::default().fg(quiet)));
        } else {
            let (icon, icon_color) = crate::file_icons::for_path(&entry.path, associations);
            let icon_color = ui.readable_foreground(icon_color, row_bg, 3.0);
            spans.push(Span::styled(icon, Style::default().fg(icon_color)));
            // The extension is quieter than the name.
            let (stem, ext) = match name.rfind('.') {
                Some(d) if d > 0 => name.split_at(d),
                _ => (name.as_str(), ""),
            };
            let fg = if hidden && !selected { quiet } else { text_fg };
            spans.push(Span::styled(stem.to_string(), Style::default().fg(fg)));
            spans.push(Span::styled(ext.to_string(), Style::default().fg(quiet)));
        }
        if selected {
            // Fill the whole row so the selection reads as a bar.
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
        }
        let row = Rect::new(
            dir.list_area.x,
            dir.list_area.y + i as u16,
            dir.list_area.width,
            1,
        );
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(row_bg)),
            row,
        );
    }
    if let Some((q, forward, _)) = &dir.search {
        let y = area.bottom() - 1;
        let prompt = format!("{}{q}", if *forward { '/' } else { '?' });
        f.render_widget(
            Paragraph::new(Span::styled(prompt, Style::default().fg(colors.fg_text)))
                .style(Style::default().bg(bg)),
            Rect::new(area.x, y, area.width, 1),
        );
    } else if hint_rows > 0 {
        let y = area.bottom() - 1;
        let key = Style::default().fg(colors.fg_text);
        let text = Style::default().fg(dim);
        let hint = Line::from(vec![
            Span::styled(" j/k", key),
            Span::styled(" move  ", text),
            Span::styled("Enter", key),
            Span::styled(" open  ", text),
            Span::styled("/", key),
            Span::styled(" find  ", text),
            Span::styled("%", key),
            Span::styled(" new file  ", text),
            Span::styled("-", key),
            Span::styled(" parent  ", text),
            Span::styled("q", key),
            Span::styled(" close", text),
        ]);
        f.render_widget(
            Paragraph::new(hint).style(Style::default().bg(bg)),
            Rect::new(area.x, y, area.width, 1),
        );
        let pos = format!("{}/{} ", dir.selected + 1, dir.entries.len());
        let w = pos.chars().count() as u16;
        if w + 40 < area.width {
            f.render_widget(
                Paragraph::new(Span::styled(pos, text)),
                Rect::new(area.right() - w, y, w, 1),
            );
        }
    }
}
