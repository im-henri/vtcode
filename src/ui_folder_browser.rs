//! Explorer-style folder selection inside the terminal workbench.
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::folder_browser::{Action, Focus};
use crate::text_input::TextInputId;
use crate::ui_pickers::truncate_to_width;
use crate::ui_widgets::InputField;

#[derive(Debug)]
struct BrowserLayout {
    title: Rect,
    navigation: Rect,
    tools: Rect,
    sidebar: Rect,
    header: Rect,
    list: Rect,
    status: Rect,
    folder: Rect,
    actions: Rect,
}

fn layout(area: Rect) -> BrowserLayout {
    let roomy = area.height >= 16;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(if roomy { 3 } else { 1 }),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(if roomy { 3 } else { 1 }),
            Constraint::Length(1),
        ])
        .split(area);
    let side_width = if area.width >= 90 {
        24
    } else if area.width >= 60 {
        20
    } else {
        0
    };
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(side_width), Constraint::Min(0)])
        .split(rows[3]);
    let header_height = body[1].height.min(1);
    BrowserLayout {
        title: rows[0],
        navigation: rows[1],
        tools: rows[2],
        sidebar: body[0],
        header: Rect::new(body[1].x, body[1].y, body[1].width, header_height),
        list: Rect::new(
            body[1].x,
            body[1].y + header_height,
            body[1].width,
            body[1].height.saturating_sub(header_height),
        ),
        status: rows[4],
        folder: rows[5],
        actions: rows[6],
    }
}

fn label(f: &mut Frame, area: Rect, text: impl Into<String>, style: Style) {
    f.render_widget(Paragraph::new(text.into()).style(style), area);
}

fn surface_at(f: &mut Frame, app: &App, area: Rect) -> ratatui::style::Color {
    f.buffer_mut().cell((area.x, area.y)).map_or(app.colors.ui.bg_widget, |cell| cell.bg)
}

fn control(f: &mut Frame, app: &mut App, area: Rect, text: &str, action: Action, enabled: bool) {
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let surface = surface_at(f, app, area);
    let mut style = app.colors.ui.toolbar_layer_style(app.colors.ui.widget_foreground, surface,
        hovered && enabled, false, false);
    if !enabled {
        let fg = style.fg.unwrap();
        style = style.fg(crate::ui_colors::disabled_text_color(
            app.colors.ui.disabled_foreground.over(surface).unwrap_or(fg), fg, surface, app.colors.ui.contrast_colors()));
    }
    label(f, area, text, style);
    if enabled && !area.is_empty() {
        app.folder_browser.controls.push((area, action));
    }
}

fn input(f: &mut Frame, app: &mut App, area: Rect, id: TextInputId, placeholder: &str) {
    if area.is_empty() {
        return;
    }
    let focused = app.folder_browser.focus.input() == Some(id);
    let style = app.colors.ui.input_style(surface_at(f, app, area));
    let bg = style.bg.unwrap();
    let fg = style.fg.unwrap();
    let border = (if focused { app.colors.ui.focus_border } else { app.colors.ui.input_colors.border })
        .border(bg).unwrap_or(bg);
    let mut inner = if area.height >= 3 {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border))
            .style(style);
        let inner = block.inner(area);
        f.render_widget(block, area);
        inner
    } else {
        area
    };
    if id == TextInputId::FolderSearch && !app.folder_browser.search.is_empty() {
        inner.width = inner.width.saturating_sub(2);
    }
    let (text, cursor, anchor) = match id {
        TextInputId::FolderAddress => (
            &app.change_dir_query,
            app.folder_browser.address_cursor,
            app.folder_browser.address_anchor,
        ),
        TextInputId::FolderSearch => (
            &app.folder_browser.search,
            app.folder_browser.search_cursor,
            app.folder_browser.search_anchor,
        ),
        _ => (
            &app.folder_browser.folder,
            app.folder_browser.folder_cursor,
            app.folder_browser.folder_anchor,
        ),
    };
    let mut field = InputField::new(text)
        .theme_colors(&app.colors)
        .prefix(" ", fg)
        .bg(bg)
        .placeholder_text(placeholder)
        .cursor_marker(if focused { "│" } else { "" })
        .cursor_pos(cursor.min(text.len()));
    if focused {
        field = field.selection_range(
            App::inline_selection_range(cursor, anchor),
        );
    }
    let map = field.render(f, inner);
    app.register_text_input(id, map);
}

fn breadcrumbs(f: &mut Frame, app: &mut App, area: Rect) {
    let style = app.colors.ui.input_style(surface_at(f, app, area));
    let bg = style.bg.unwrap();
    let description = app.colors.ui.description_color(bg);
    let block = Block::default()
        .borders(if area.height >= 3 {
            Borders::ALL
        } else {
            Borders::NONE
        })
        .border_style(Style::default().fg(app.colors.ui.input_colors.border.border(bg).unwrap_or(bg)))
        .style(style);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.folder_browser.controls.push((area, Action::Address));
    if inner.width < 5 {
        return;
    }
    let mut parts: Vec<_> = if app.folder_browser_is_computer() {
        Vec::new()
    } else if let Some(remote) = &app.remote_workspace {
        remote.breadcrumbs(&app.folder_browser_path).unwrap_or_default()
    } else {
        let mut parts: Vec<_> = app
            .folder_browser_path
            .ancestors()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string());
                (name, p.to_path_buf())
            })
            .collect();
        parts.reverse();
        parts
    };
    parts.insert(0, ("Computer".into(), std::path::PathBuf::new()));
    let mut total: usize = parts.iter().map(|(n, _)| n.width() + 3).sum();
    let budget = inner.width.saturating_sub(4) as usize;
    let mut skipped = false;
    while parts.len() > 1 && total > budget {
        total = total.saturating_sub(parts.remove(0).0.width() + 3);
        skipped = true;
    }
    let mut x = inner.x + 1;
    if skipped {
        label(
            f,
            Rect::new(x, inner.y, 2.min(inner.width), 1),
            "… ",
            Style::default().fg(description).bg(bg),
        );
        x += 2;
    }
    let last = parts.len().saturating_sub(1);
    for (i, (name, path)) in parts.into_iter().enumerate() {
        let width = (name.width() as u16).min(inner.right().saturating_sub(x + 2));
        if width == 0 {
            break;
        }
        let area = Rect::new(x, inner.y, width, 1);
        let hovered = area.contains((app.mouse_col, app.mouse_row).into());
        label(
            f,
            area,
            truncate_to_width(&name, width as usize),
            app.colors.ui.toolbar_layer_style(if i == last { app.colors.ui.input_colors.foreground }
                else { app.colors.ui.description_foreground }, bg, hovered, false, false)
                .add_modifier(if i == last {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        );
        app.folder_browser
            .controls
            .push((area, Action::Navigate(path)));
        x += width;
        if i != last && x + 3 <= inner.right() {
            label(
                f,
                Rect::new(x, inner.y, 3, 1),
                " › ",
                Style::default().fg(description).bg(bg),
            );
            x += 3;
        }
    }
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        app.folder_browser.location_area = Rect::default();
        return;
    }
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(app.colors.ui.fg_group_border))
        .style(Style::default().bg(app.colors.bg_sidebar));
    let inner = block.inner(area);
    f.render_widget(block, area);
    label(
        f,
        Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1),
        "QUICK ACCESS",
        Style::default()
            .fg(app.colors.ui.description_color(app.colors.bg_sidebar))
            .add_modifier(Modifier::BOLD),
    );
    let rows = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    app.folder_browser.location_area = rows;
    let max = app
        .folder_browser
        .locations
        .len()
        .saturating_sub(rows.height as usize);
    app.folder_browser.location_scroll = app.folder_browser.location_scroll.min(max);
    let start = app.folder_browser.location_scroll;
    for (i, (name, path)) in app
        .folder_browser
        .locations
        .iter()
        .enumerate()
        .skip(start)
        .take(rows.height as usize)
    {
        let row = Rect::new(rows.x, rows.y + (i - start) as u16, rows.width, 1);
        let active = app.folder_browser.focus == Focus::Locations;
        let selected = if active { i == app.folder_browser.location_selected }
            else { path == &app.folder_browser_path };
        let hovered = row.contains((app.mouse_col, app.mouse_row).into());
        let style = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground,
            app.colors.bg_sidebar, selected, active, hovered);
        let bg = style.bg.unwrap();
        let icon = match name.as_str() {
            "Computer" => "▣",
            "Home" => "⌂",
            "Desktop" => "▣",
            "Downloads" => "↓",
            "Documents" => "≡",
            "Pictures" => "▧",
            "Music" => "♪",
            "Videos" => "▷",
            "Workspace" => "◇",
            _ => "▰",
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {icon} "),
                    Style::default().fg(app.colors.ui.list_icon_color(app.colors.ui.icon_color(bg), bg, selected, active)),
                ),
                Span::styled(
                    truncate_to_width(name, rows.width.saturating_sub(4) as usize),
                    style,
                ),
            ]))
            .style(style),
            row,
        );
        app.folder_browser.controls.push((row, Action::Location(i)));
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let layout = layout(area);
    app.folder_browser.controls.clear();
    app.folder_browser_buttons.clear();
    let bg = crate::ui_widgets::widget_background(f, &app.colors, area);
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default().style(
            Style::default()
                .bg(bg)
                .fg(app.colors.ui.widget_text_color(bg)),
        ),
        area,
    );

    label(
        f,
        layout.title,
        if app.folder_browser.save_as_return { "  ▰  Choose Save Location" } else { "  ▰  Open Folder" },
        Style::default()
            .bg(bg)
            .fg(app.colors.ui.widget_text_color(bg))
            .add_modifier(Modifier::BOLD),
    );
    let close = Rect::new(
        layout.title.right().saturating_sub(4).max(layout.title.x),
        layout.title.y,
        4.min(layout.title.width),
        layout.title.height,
    );
    control(f, app, close, " × ", Action::Close, true);

    label(
        f,
        layout.navigation,
        "",
        Style::default().bg(app.colors.bg_sidebar),
    );
    let nav_width = 12.min(layout.navigation.width);
    let nav_y = layout.navigation.y + u16::from(layout.navigation.height >= 3);
    for (i, (text, action, enabled)) in [
        (" ← ", Action::Back, !app.folder_browser.back.is_empty()),
        (
            " → ",
            Action::Forward,
            !app.folder_browser.forward.is_empty(),
        ),
        (
            " ↑ ",
            Action::Up,
            app.folder_browser_parent().is_some(),
        ),
        (" ↻ ", Action::Refresh, !app.folder_browser.creating),
    ]
    .into_iter()
    .enumerate()
    {
        let x = layout.navigation.x + i as u16 * 3;
        let width = layout.navigation.right().saturating_sub(x).min(3);
        if width > 0 {
            control(f, app, Rect::new(x, nav_y, width, 1), text, action, enabled);
        }
    }
    let search_width = if area.width >= 85 {
        28
    } else if area.width >= 55 {
        22
    } else {
        0
    };
    let address = Rect::new(
        layout.navigation.x + nav_width,
        layout.navigation.y,
        layout
            .navigation
            .width
            .saturating_sub(nav_width + search_width),
        layout.navigation.height,
    );
    if app.folder_browser.focus == Focus::Address {
        input(
            f,
            app,
            address,
            TextInputId::FolderAddress,
            "Enter folder path",
        );
    } else {
        breadcrumbs(f, app, address);
    }
    if search_width > 0 {
        let search = Rect::new(address.right(), address.y, search_width, address.height);
        input(
            f,
            app,
            search,
            TextInputId::FolderSearch,
            if search_width >= 26 {
                "Search folders  Ctrl+F"
            } else {
                "Search folders"
            },
        );
        if !app.folder_browser.search.is_empty() {
            control(
                f,
                app,
                Rect::new(search.right().saturating_sub(3), nav_y, 2, 1),
                "× ",
                Action::ClearSearch,
                true,
            );
        }
    }

    label(
        f,
        layout.tools,
        "",
        Style::default().bg(app.colors.bg_sidebar),
    );
    let new_width = layout.tools.width.min(16);
    control(
        f,
        app,
        Rect::new(
            layout.tools.x,
            layout.tools.y,
            new_width,
            layout.tools.height,
        ),
        " + New folder ",
        Action::NewFolder,
        !app.folder_browser_loading && !app.folder_browser.creating && !app.folder_browser_is_computer(),
    );
    let hidden_width = layout.tools.width.saturating_sub(new_width).min(21);
    if search_width > 0 {
        control(
            f,
            app,
            Rect::new(
                layout.tools.x + new_width,
                layout.tools.y,
                hidden_width,
                layout.tools.height,
            ),
            if app.folder_browser.show_hidden {
                " [x] Hidden folders "
            } else {
                " [ ] Hidden folders "
            },
            Action::Hidden,
            true,
        );
    }
    if search_width == 0 {
        let search = Rect::new(
            layout.tools.x + new_width,
            layout.tools.y,
            layout.tools.width.saturating_sub(new_width),
            layout.tools.height,
        );
        input(f, app, search, TextInputId::FolderSearch, "Search folders");
        app.folder_browser
            .controls
            .retain(|(_, a)| !matches!(a, Action::Hidden));
    } else if layout.tools.width > 72 {
        label(
            f,
            Rect::new(layout.tools.right() - 33, layout.tools.y, 32, 1),
            "Ctrl+L  Path   Ctrl+Shift+N  New",
            Style::default().fg(app.colors.ui.description_color(app.colors.bg_sidebar)),
        );
    }

    draw_sidebar(f, app, layout.sidebar);
    let details = layout.list.width >= 54;
    let type_column = layout.list.width >= 72;
    let name_width = layout
        .list
        .width
        .saturating_sub(if details { 20 } else { 1 })
        .saturating_sub(if type_column { 14 } else { 0 });
    let name_header = Rect::new(
        layout.header.x,
        layout.header.y,
        name_width,
        layout.header.height,
    );
    control(
        f,
        app,
        name_header,
        if !app.folder_browser.sort_modified && app.folder_browser.descending {
            "  Name ↓"
        } else if !app.folder_browser.sort_modified {
            "  Name ↑"
        } else {
            "  Name"
        },
        Action::SortName,
        true,
    );
    if details {
        control(
            f,
            app,
            Rect::new(
                name_header.right(),
                layout.header.y,
                20,
                layout.header.height,
            ),
            if app.folder_browser.sort_modified && app.folder_browser.descending {
                " Modified ↓"
            } else if app.folder_browser.sort_modified {
                " Modified ↑"
            } else {
                " Modified"
            },
            Action::SortModified,
            true,
        );
    }
    if type_column {
        label(
            f,
            Rect::new(
                layout.header.right().saturating_sub(14),
                layout.header.y,
                14,
                layout.header.height,
            ),
            " Type",
            Style::default()
                .fg(app.colors.ui.description_color(bg))
                .bg(bg),
        );
    }

    let resized = app.folder_browser_area.is_none_or(|previous| {
        previous.width != layout.list.width || previous.height != layout.list.height
    });
    app.folder_browser_area = Some(layout.list);
    let indices = app.folder_browser_indices();
    let parent = usize::from(app.folder_browser_parent().is_some());
    let count = indices.len() + parent;
    if resized && app.folder_browser.focus == Focus::List && layout.list.height > 0 {
        let visible = layout.list.height as usize;
        app.folder_browser_scroll = app.folder_browser_scroll.min(app.folder_browser_selected);
        if app.folder_browser_selected >= app.folder_browser_scroll + visible {
            app.folder_browser_scroll = app.folder_browser_selected + 1 - visible;
        }
    }
    app.folder_browser_scroll = app
        .folder_browser_scroll
        .min(count.saturating_sub(layout.list.height.max(1) as usize));
    let scroll = app.folder_browser_scroll;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    for row_index in 0..layout.list.height as usize {
        let index = scroll + row_index;
        if index >= count {
            break;
        }
        let row = Rect::new(
            layout.list.x,
            layout.list.y + row_index as u16,
            layout.list.width,
            1,
        );
        let active = app.folder_browser.focus == Focus::List;
        let selected = index == app.folder_browser_selected;
        let hovered = row.contains((app.mouse_col, app.mouse_row).into());
        let style = app.colors.ui.list_row_style(app.colors.ui.widget_foreground,
            bg, selected, active, hovered);
        let bg = style.bg.unwrap();
        let fg = style.fg.unwrap();
        let description = if selected || hovered { fg } else { app.colors.ui.description_color(bg) };
        label(f, row, "", style);
        let is_parent = parent == 1 && index == 0;
        let path = if is_parent {
            None
        } else {
            indices
                .get(index - parent)
                .map(|&i| &app.folder_browser_entries[i])
        };
        let name = path.map(|path| app.folder_browser_entry_name(path).into_owned())
            .unwrap_or_else(|| if app.folder_browser_parent().is_some_and(|parent| parent.as_os_str().is_empty()) {
                "..  Computer".into()
            } else { "..  Parent folder".into() });
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if is_parent { " ↑ " } else { " ▰ " },
                    Style::default().fg(app.colors.ui.list_icon_color(
                        app.colors.ui.folder_icon_color(bg), bg, selected, active,
                    )),
                ),
                Span::styled(
                    ellipsize(&name, name_width.saturating_sub(4) as usize),
                    Style::default().fg(fg),
                ),
            ]))
            .style(style),
            Rect::new(row.x, row.y, name_width, 1),
        );
        if details {
            let modified = path
                .and_then(|p| app.folder_browser.modified.get(p))
                .map(|time| {
                    let then = time
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    App::format_time_ago(now, then)
                })
                .unwrap_or_else(|| "—".into());
            label(
                f,
                Rect::new(row.x + name_width, row.y, 20, 1),
                format!(" {modified}"),
                Style::default()
                    .fg(description)
                    .bg(bg),
            );
        }
        if type_column {
            label(
                f,
                Rect::new(row.right().saturating_sub(14), row.y, 13, 1),
                if is_parent { "" } else if app.folder_browser_is_computer() { " File system" } else { " File folder" },
                Style::default()
                    .fg(description)
                    .bg(bg),
            );
        }
    }
    if app.folder_browser_loading || indices.is_empty() {
        if app.folder_browser_loading {
            crate::ui_loading::draw_header(f, app, layout.header);
        }
        let message = if app.folder_browser_loading {
            if app.folder_browser_is_computer() { "Loading file systems…" } else {
            "Loading folders…"
            }
        } else if app.folder_browser.error.is_some() {
            "This folder could not be fully listed."
        } else if !app.folder_browser.search.is_empty() {
            if app.folder_browser_is_computer() { "No file systems match your search." }
            else { "No folders match your search." }
        } else if app.folder_browser_is_computer() {
            "No file systems are available."
        } else {
            "This folder has no subfolders."
        };
        let hint = if !app.folder_browser.search.is_empty() {
            "Clear the search to see all folders."
        } else {
            "Use Select folder to open this location."
        };
        let y = layout.list.y
            + (layout.list.height / 2)
                .max(parent as u16)
                .min(layout.list.height.saturating_sub(1));
        if layout.list.height > 0 {
            f.render_widget(
                Paragraph::new(message)
                    .centered()
                    .style(Style::default().fg(app.colors.ui.description_color(bg))),
                Rect::new(layout.list.x, y, layout.list.width, 1),
            );
            if y + 1 < layout.list.bottom() {
                f.render_widget(
                    Paragraph::new(hint)
                        .centered()
                        .style(Style::default().fg(app.colors.ui.description_color(bg))),
                    Rect::new(layout.list.x, y + 1, layout.list.width, 1),
                );
            }
        }
    }
    if count > layout.list.height as usize && !layout.list.is_empty() {
        let hovered = app.mouse_col == layout.list.right().saturating_sub(1) && (layout.list.y..layout.list.bottom()).contains(&app.mouse_row);
        let (track, thumb) = app.colors.ui.scrollbar_colors(bg, hovered, false);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(thumb).bg(track))
                .track_style(Style::default().fg(track).bg(track)),
            layout.list,
            &mut ScrollbarState::new(count)
                .position(scroll)
                .viewport_content_length(layout.list.height as usize),
        );
    }

    let noun = if app.folder_browser_is_computer() { "locations" } else { "folders" };
    let status = if let Some(error) = &app.folder_browser.error {
        format!("  {error}")
    } else if app.folder_browser.creating {
        "  Creating folder…".into()
    } else if app.folder_browser_loading {
        "  Reading folder…".into()
    } else if app.folder_browser.new_folder {
        "  Choose a name for the new folder.".into()
    } else if app.folder_browser.search.is_empty() {
        format!("  {} {noun}  ·  Double-click to browse", indices.len())
    } else {
        format!(
            "  {} of {} {noun}  ·  Search in this location",
            indices.len(),
            app.folder_browser_entries.len()
        )
    };
    label(
        f,
        layout.status,
        status,
        Style::default()
            .bg(app.colors.bg_sidebar)
            .fg(if app.folder_browser.error.is_some() {
                app.colors.ui.error_text_color(app.colors.bg_sidebar)
            } else {
                app.colors.ui.description_color(app.colors.bg_sidebar)
            }),
    );
    label(
        f,
        layout.folder,
        "",
        Style::default().bg(app.colors.bg_sidebar),
    );
    let prefix = if app.folder_browser.new_folder {
        " New folder: "
    } else {
        " Folder: "
    };
    let prefix_width = (prefix.width() as u16).min(layout.folder.width);
    label(
        f,
        Rect::new(
            layout.folder.x,
            layout.folder.y + u16::from(layout.folder.height >= 3),
            prefix_width,
            layout.folder.height.min(1),
        ),
        prefix,
        Style::default().fg(app.colors.ui.widget_text_color(app.colors.bg_sidebar)),
    );
    input(
        f,
        app,
        Rect::new(
            layout.folder.x + prefix_width,
            layout.folder.y,
            layout.folder.width.saturating_sub(prefix_width + 1),
            layout.folder.height,
        ),
        TextInputId::FolderName,
        "Folder path",
    );

    label(
        f,
        layout.actions,
        "",
        Style::default().bg(app.colors.bg_sidebar),
    );
    let primary = if app.folder_browser.creating {
        " Creating… "
    } else if app.folder_browser.new_folder {
        " Create folder "
    } else if app.folder_browser.save_as_return {
        " Use folder "
    } else if area.width >= 45 {
        " Select folder "
    } else {
        " Select "
    };
    let secondary = " Cancel ";
    if app.folder_browser.creating {
        app.folder_browser.focus = Focus::Cancel;
    }
    let total = (primary.trim().width() + secondary.trim().width() + 10) as u16;
    let mut x = layout
        .actions
        .right()
        .saturating_sub(total)
        .max(layout.actions.x);
    for (i, text) in [primary, secondary].into_iter().enumerate() {
        let width = (text.trim().width() as u16 + 4).min(layout.actions.right().saturating_sub(x));
        let rect = Rect::new(x, layout.actions.y, width, layout.actions.height);
        let focused = app.folder_browser.focus == if i == 0 { Focus::Open } else { Focus::Cancel };
        let enabled = i != 0 || (!app.folder_browser.creating
            && (!app.folder_browser_is_computer() || !app.folder_browser.folder.is_empty()));
        let mut style = app.colors.ui.button_style(app.colors.bg_sidebar, i == 0,
            rect.contains((app.mouse_col, app.mouse_row).into()), enabled);
        if focused && enabled {
            style = style.add_modifier(Modifier::BOLD);
            if let Some(border) = app.colors.ui.focus_border.border(style.bg.unwrap()) {
                style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
            }
        }
        crate::ui_widgets::draw_button_content(f, rect, text, style, focused && enabled, false);
        app.folder_browser_buttons
            .push((rect.x, rect.y, rect.width));
        x = x.saturating_add(width + 2);
    }
}

fn ellipsize(text: &str, width: usize) -> String {
    if text.width() <= width {
        text.into()
    } else if width == 0 {
        String::new()
    } else {
        format!("{}…", truncate_to_width(text, width - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::PathBuf;

    fn fixture() -> App {
        let mut app = App::test_new();
        app.change_dir_active = true;
        app.folder_browser_path = PathBuf::from("example-workspace/projects");
        app.change_dir_query = app.folder_browser_path.display().to_string();
        app.folder_browser.folder = app.change_dir_query.clone();
        app.folder_browser.folder_cursor = app.folder_browser.folder.len();
        let names = [
            "Application",
            "An Empty Project",
            "Parser Example",
            "Audio Example",
            "Core Library",
            "Console Game",
            "Compiler Example",
            "Design resources",
            "docs",
            "examples",
            "extensions",
            "Rendering Example",
            "src",
            "themes",
            "vtcode",
            "Workspace with a long name 你好",
        ];
        let now = std::time::SystemTime::now();
        for (i, name) in names.into_iter().enumerate() {
            let path = app.folder_browser_path.join(name);
            app.folder_browser.modified.insert(
                path.clone(),
                now - std::time::Duration::from_secs(86400 * (i as u64 + 1)),
            );
            app.folder_browser_entries.push(path);
        }
        for name in [
            "Home",
            "Desktop",
            "Downloads",
            "Documents",
            "Pictures",
            "Music",
            "Videos",
            "Workspace",
            "Local disk (C:)",
        ] {
            app.folder_browser
                .locations
                .push((name.into(), PathBuf::from(format!("C:/{name}"))));
        }
        app.select_browser_row(5);
        app
    }

    fn render_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn computer_page_renders_drive_labels_and_disables_folder_creation() {
        let mut app = App::test_new();
        app.change_dir_active = true;
        app.folder_browser_path = PathBuf::new();
        let roots: Vec<_> = if cfg!(windows) {
            [r"C:\", r"Z:\"].map(PathBuf::from).into()
        } else { ["/", "/media/second"].map(PathBuf::from).into() };
        app.folder_browser.locations = vec![
            ("Computer".into(), PathBuf::new()),
            ("Disk (C:)".into(), roots[0].clone()),
            ("Disk (Z:)".into(), roots[1].clone()),
        ];
        app.folder_browser_entries = roots.clone();
        app.filter_folder_browser();
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = render_text(&terminal);
        assert!(text.contains("Computer"));
        assert!(text.contains("Disk (C:)"));
        assert!(text.contains("Disk (Z:)"));
        assert!(text.contains("2 locations"));
        assert!(!text.contains("Parent folder"));
        assert!(!app.folder_browser.controls.iter().any(|(_, action)| matches!(action, Action::NewFolder | Action::Up)));
        app.folder_browser.search = "Z:".into();
        app.filter_folder_browser();
        assert_eq!(&*app.folder_browser_indices(), &[1]);
        app.select_browser_row(0);
        assert_eq!(app.folder_browser.folder, app.display_path(&roots[1]));
        app.folder_browser_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('n'),
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT));
        assert!(!app.folder_browser.new_folder);
    }

    #[test]
    fn remote_folder_browser_breadcrumbs_show_node_paths_and_keep_remote_targets() {
        use crate::remote::{self, RemotePath};
        let hello = remote::Hello {
            version: remote::VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            root: "/home/henri/project space".into(),
            root_uri: "file:///home/henri/project%20space/".into(),
            session: 42,
        };
        let client = remote::client::Client::attach(
            std::io::Cursor::new(Vec::new()), Vec::new(), hello,
        ).unwrap();
        let remote = remote::workspace::Workspace::new(client);
        let mut app = App::test_new();
        app.remote_workspace = Some(remote.clone());
        let mut terminal = Terminal::new(TestBackend::new(180, 30)).unwrap();
        for (path, expected) in [
            (RemotePath::default(), vec!["/", "home", "henri", "project space"]),
            (RemotePath::new(b"src/child".to_vec()).unwrap(),
                vec!["/", "home", "henri", "project space", "src", "child"]),
            (RemotePath::absolute(b"/home/henri/other").unwrap(),
                vec!["/", "home", "henri", "other"]),
            (RemotePath::absolute(b"/").unwrap(), vec!["/"]),
        ] {
            app.folder_browser_path = remote.view_key(&path);
            app.folder_browser.folder = app.display_path(&app.folder_browser_path);
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let parts = remote.breadcrumbs(&app.folder_browser_path).unwrap();
            assert_eq!(parts.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), expected);
            let targets: Vec<_> = app.folder_browser.controls.iter().filter_map(|(area, action)| {
                let Action::Navigate(target) = action else { return None; };
                let label: String = (area.x..area.right())
                    .map(|x| terminal.backend().buffer()[(x, area.y)].symbol()).collect();
                Some((label, target.clone()))
            }).collect();
            let mut expected_targets = vec![("Computer".to_string(), PathBuf::new())];
            expected_targets.extend(parts);
            assert_eq!(targets, expected_targets);
            assert_eq!(targets.last().unwrap().1, app.folder_browser_path);
            let text = render_text(&terminal);
            assert!(!text.contains("vtcode-remote-"));
            assert!(!text.contains("~host"));
            for (i, (_, target)) in targets.iter().skip(1).enumerate() {
                let uri = url::Url::from_file_path(target).unwrap();
                let uri = remote.lsp_uri(uri.as_str(), true).unwrap();
                let expected = if i == 0 { "file:///".into() }
                    else { format!("file:///{}", expected[1..=i].join("/").replace(' ', "%20")) };
                assert_eq!(uri.trim_end_matches('/'), expected.trim_end_matches('/'));
            }
            app.folder_browser_action(Action::Address);
            assert_eq!(remote.target_key(&app.change_dir_query).unwrap(), app.folder_browser_path);
            app.folder_browser.focus = Focus::List;
        }
    }

    #[test]
    fn folder_dialog_uses_widget_sidebar_input_and_button_roles_on_own_surfaces() {
        use crate::theme_workbench::ColorLayer;
        use ratatui::style::Color;
        let mut app = fixture();
        app.colors = crate::ui_control_visibility_tests::theme_with_popup_and_control_roles().to_theme_colors();
        app.mouse_col = u16::MAX;
        app.mouse_row = u16::MAX;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let widget = Color::Rgb(32, 40, 48);
        let sidebar = Color::Rgb(24, 48, 37);
        let geometry = layout(Rect::new(0, 0, 100, 30));
        for focus in [Focus::List, Focus::Address] {
            app.folder_browser.focus = focus;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(6, 0)].bg, widget);
            assert_eq!(buffer[(6, 0)].fg, ColorLayer::rgba(255, 238, 221, 224).over(widget).unwrap());
            let home = &buffer[(3, geometry.sidebar.y + 1)];
            assert_eq!(home.symbol(), "H");
            assert_eq!(home.bg, sidebar);
            assert_eq!(home.fg, ColorLayer::rgba(221, 238, 255, 192).over(sidebar).unwrap());
            let input = &buffer[(74, geometry.navigation.y + 1)];
            let input_bg = ColorLayer::rgba(48, 56, 80, 192).over(sidebar).unwrap();
            assert_eq!(input.bg, input_bg);
            assert_eq!(input.fg, ColorLayer::rgba(221, 238, 255, 224).over(input_bg).unwrap());
            let row = geometry.list.y + app.folder_browser_selected as u16;
            let (fill, text) = if focus == Focus::List {
                (ColorLayer::rgba(48, 72, 56, 192), ColorLayer::rgba(255, 255, 187, 224))
            } else {
                (ColorLayer::rgba(48, 72, 104, 192), ColorLayer::rgba(187, 221, 255, 224))
            };
            let row_bg = fill.over(widget).unwrap();
            assert_eq!(buffer[(geometry.list.x + 4, row)].bg, row_bg);
            assert_eq!(buffer[(geometry.list.x + 4, row)].fg, text.over(row_bg).unwrap());
            assert_eq!(buffer[(geometry.list.right() - 2, row)].bg, row_bg);
            let (x, y, _) = app.folder_browser_buttons[0];
            assert_eq!(buffer[(x + 2, y)].bg, Color::Rgb(32, 59, 112));
            assert_eq!(buffer[(x + 2, y)].fg, Color::Rgb(255, 244, 187));
            if focus == Focus::List {
                crate::ui_control_visibility_tests::snapshot(buffer, "independent folder dialog roles");
            }
        }
    }

    #[test]
    fn folder_dialog_text_and_controls_remain_readable_across_loaded_themes() {
        use crate::ui_control_visibility_tests::{assert_readable, audit_themes, check_region};
        let mut app = fixture();
        app.folder_browser.back.push(app.folder_browser_path.clone());
        app.folder_browser.forward.push(app.folder_browser_path.clone());
        let mut failures = Vec::new();
        for theme in audit_themes() {
            app.colors = theme.to_theme_colors();
            for width in [45, 100] {
                let geometry = layout(Rect::new(0, 0, width, 30));
                let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                for focus in [Focus::List, Focus::Address, Focus::Locations, Focus::Open] {
                    app.folder_browser.focus = focus;
                    app.mouse_col = geometry.list.x + 5;
                    app.mouse_row = geometry.list.y + 1;
                    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                    let buffer = terminal.backend().buffer();
                    let label = format!("{} folder dialog, width={width}, focus={focus:?}", theme.name);
                    for region in [
                        Rect::new(5, 0, width.saturating_sub(9), 1),
                        Rect::new(0, geometry.tools.y, width, 1),
                        Rect::new(geometry.list.x + 3, geometry.list.y,
                            geometry.list.width.saturating_sub(4), geometry.list.height),
                        geometry.status, geometry.actions,
                        Rect::new(0, geometry.folder.y + 1, 9, 1),
                        Rect::new(10, geometry.folder.y + 1, width.saturating_sub(12), 1),
                    ] {
                        check_region(buffer, region, &label, &mut failures, &app.colors.ui);
                    }
                    if geometry.sidebar.width > 0 {
                        check_region(buffer, Rect::new(3, geometry.sidebar.y,
                            geometry.sidebar.width.saturating_sub(4), geometry.sidebar.height),
                            &label, &mut failures, &app.colors.ui);
                    }
                    let navigation = geometry.navigation;
                    let search_width = if width >= 85 { 28 } else if width >= 55 { 22 } else { 0 };
                    for region in [
                        Rect::new(1, navigation.y + 1, 10, 1),
                        Rect::new(13, navigation.y + 1, width.saturating_sub(search_width + 14), 1),
                        Rect::new(width.saturating_sub(search_width).saturating_add(1), navigation.y + 1,
                            search_width.saturating_sub(2), 1),
                    ] {
                        check_region(buffer, region, &label, &mut failures, &app.colors.ui);
                    }
                }
            }
        }
        assert_readable(failures);
    }

    #[test]
    fn folder_dialog_buttons_share_pointer_and_keyboard_focus() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
        let mut app = fixture();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let (x, y, _) = app.folder_browser_buttons[1];
        let hover = MouseEvent { kind: MouseEventKind::Moved, column: x + 1, row: y, modifiers: KeyModifiers::NONE };
        crate::mouse::handle_mouse(&mut app, hover);
        assert_eq!(app.folder_browser.focus, Focus::Cancel);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(render_text(&terminal).contains("[ Cancel ]"));
        assert!(!render_text(&terminal).contains("Ctrl+Enter"));
        for key in [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::Down] {
            let previous = app.folder_browser.focus;
            app.folder_browser_key(KeyEvent::new(key, KeyModifiers::NONE));
            assert_ne!(app.folder_browser.focus, previous);
        }
        app.folder_browser_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        crate::mouse::handle_mouse(&mut app, hover);
        assert_eq!(app.folder_browser.focus, Focus::Open);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = render_text(&terminal);
        assert!(text.contains("[ Select folder ]"));
        assert!(!text.contains("[ Cancel ]"));
        crate::mouse::handle_mouse(&mut app, MouseEvent { column: x + 2, ..hover });
        app.folder_browser_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.change_dir_active);

        let mut app = fixture();
        app.folder_browser.creating = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.folder_browser.focus, Focus::Cancel);
        let (x, y, _) = app.folder_browser_buttons[0];
        crate::mouse::handle_mouse(&mut app, MouseEvent { column: x, row: y, ..hover });
        for key in [KeyCode::Tab, KeyCode::BackTab, KeyCode::Left, KeyCode::Right] {
            app.folder_browser_key(KeyEvent::new(key, KeyModifiers::NONE));
            assert_eq!(app.folder_browser.focus, Focus::Cancel);
        }
        app.folder_browser_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.change_dir_active, "the enabled Cancel action must still work while creating");
    }

    #[test]
    fn folder_browser_responsive_render_and_hit_regions() {
        let mut app = fixture();
        for (width, height) in [
            (120, 34),
            (100, 30),
            (80, 24),
            (60, 16),
            (40, 12),
            (20, 8),
            (3, 3),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let list = app.folder_browser_area.unwrap();
            assert!(list.right() <= width && list.bottom() <= height);
            if list.height > 0 {
                assert!(app.folder_browser_selected >= app.folder_browser_scroll);
                assert!(
                    app.folder_browser_selected < app.folder_browser_scroll + list.height as usize
                );
            }
            for (region, _) in &app.folder_browser.controls {
                if !region.is_empty() {
                    assert!(
                        region.right() <= width && region.bottom() <= height,
                        "{width}x{height}: {region:?}"
                    );
                }
            }
            if width >= 80 {
                let text = render_text(&terminal);
                assert!(text.contains("QUICK ACCESS"));
                assert!(text.contains("Modified"));
                assert!(text.contains("Select folder"));
                assert!(list.height > 12);
            }
            if width >= 40 {
                if let Ok(directory) = std::env::var("VTCODE_FOLDER_BROWSER_SNAPSHOT") {
                    std::fs::create_dir_all(&directory).unwrap();
                    let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|c| serde_json::json!({
                        "text": c.symbol(), "fg": format!("{:?}", c.fg), "bg": format!("{:?}", c.bg), "bold": c.modifier.contains(Modifier::BOLD),
                    })).collect();
                    std::fs::write(
                        PathBuf::from(directory)
                            .join(format!("folder-browser-{width}x{height}.json")),
                        serde_json::to_vec(
                            &serde_json::json!({"width": width, "height": height, "cells": cells}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
            }
        }
    }

    #[test]
    fn folder_browser_empty_search_clear_and_keyboard_focus() {
        let mut app = fixture();
        app.folder_browser.search = "missing".into();
        app.folder_browser.search_cursor = 7;
        app.filter_folder_browser();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(render_text(&terminal).contains("No folders match your search"));
        let clear = app
            .folder_browser
            .controls
            .iter()
            .find(|(_, a)| matches!(a, Action::ClearSearch))
            .unwrap()
            .0;
        crate::mouse::handle_mouse(
            &mut app,
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: clear.x,
                row: clear.y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert!(app.folder_browser.search.is_empty());
        app.folder_browser.focus = Focus::Open;
        app.folder_browser_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.folder_browser.focus, Focus::Cancel);
    }
}
