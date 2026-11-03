//! Remote Explorer layout; drawing never reads the target file or contacts SSH.
use super::*;
use crate::app_remote_targets::ConnectionStatus;

fn wrapped(f: &mut Frame, area: Rect, text: &str, style: Style) {
    if !area.is_empty() {
        f.render_widget(
            Paragraph::new(text).style(style).wrap(Wrap { trim: true }),
            area,
        );
    }
}

fn current_connection(f: &mut Frame, app: &mut App, area: Rect, y: u16) -> u16 {
    let Some(remote) = &app.remote_workspace else {
        return y;
    };
    if area.bottom().saturating_sub(y) < 5 {
        return y;
    }
    let connected = remote.client.connected();
    let recipe = remote.recipe.as_ref().and_then(|recipe| recipe.target());
    let saved = app
        .remote_targets
        .store
        .as_ref()
        .and_then(|store| {
            store.targets.iter().find(|target| {
                target.launch().is_ok_and(|launch| {
                    remote
                        .recipe
                        .as_ref()
                        .is_some_and(|recipe| recipe.matches(&launch))
                })
            })
        })
        .cloned();
    let name = saved
        .as_ref()
        .or(recipe.as_ref())
        .map_or("SSH", |target| target.name.as_str())
        .to_string();
    let path = app.display_path(&app.explorer.root);
    let bg = app.colors.bg_sidebar;
    let fg = app.colors.ui.sidebar_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    let style = Style::default().fg(fg).bg(bg);
    let detailed = area.bottom().saturating_sub(y) >= 12;
    let close_width = if area.width >= 19 {
        9
    } else {
        3.min(area.width)
    };
    let label_width = area.width.saturating_sub(close_width + 1);
    line(
        f,
        Rect::new(area.x, y, label_width, 1),
        y,
        format!("{} {name}", if connected { "●" } else { "○" }),
        style.add_modifier(Modifier::BOLD),
    );
    app.remote_targets
        .hits
        .push((Rect::new(area.x, y, label_width, 1), Action::Status));
    button(
        f,
        app,
        Rect::new(area.right() - close_width, y, close_width, 1),
        if close_width >= 9 { "Close" } else { "×" },
        Action::Disconnect,
        false,
        false,
        false,
    );
    if detailed {
        line(
            f,
            area,
            y + 1,
            if connected {
                format!("Connected · {path}")
            } else {
                format!("Disconnected · {path}")
            },
            style.fg(dim),
        );
        button(
            f,
            app,
            Rect::new(area.x, y + 2, area.width.min(15), 1),
            if saved.is_some() {
                "Connection"
            } else {
                "Save Target"
            },
            if saved.is_some() {
                Action::Status
            } else {
                Action::SaveCurrent
            },
            false,
            false,
            false,
        );
        y + 4
    } else {
        y + 2
    }
}

pub(crate) fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    app.remote_targets.hits.clear();
    app.remote_targets.list_area = Rect::default();
    app.remove_text_input_regions(|id| id == TextInputId::RemoteFilter);
    let bg = app.colors.bg_sidebar;
    let fg = app.colors.ui.sidebar_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    let style = Style::default().fg(fg).bg(bg);
    f.render_widget(Paragraph::new("").style(style), area);
    let inner = Rect::new(
        area.x + u16::from(app.sidebar_right),
        area.y,
        area.width.saturating_sub(1),
        area.height,
    );
    if inner.is_empty() {
        return;
    }
    let content = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    if content.is_empty() {
        return;
    }
    let toolbar = if content.width >= 18 {
        11
    } else if content.width >= 9 {
        3
    } else {
        0
    };
    line(
        f,
        Rect::new(
            content.x,
            content.y,
            content
                .width
                .saturating_sub(toolbar + u16::from(toolbar > 0)),
            1,
        ),
        content.y,
        "REMOTE",
        style.add_modifier(Modifier::BOLD),
    );
    if toolbar > 0 {
        button(
            f,
            app,
            Rect::new(content.right() - toolbar, content.y, 3, 1),
            "+",
            Action::Add,
            false,
            false,
            false,
        );
        if toolbar > 3 {
            button(
                f,
                app,
                Rect::new(content.right() - 7, content.y, 3, 1),
                "↻",
                Action::Refresh,
                false,
                false,
                false,
            );
            button(
                f,
                app,
                Rect::new(content.right() - 3, content.y, 3, 1),
                "{}",
                Action::OpenConfig,
                false,
                false,
                false,
            );
        }
    }
    if content.height < 3 {
        return;
    }
    let compact = content.height < 11;
    let search_y = content.y + if compact { 1 } else { 2 };
    let clear = !app.remote_targets.filter.text.is_empty() && content.width >= 8;
    input(
        f,
        app,
        Rect::new(
            content.x,
            search_y,
            content.width.saturating_sub(if clear { 3 } else { 0 }),
            1,
        ),
        TextInputId::RemoteFilter,
        "Filter targets…",
    );
    if clear {
        button(
            f,
            app,
            Rect::new(content.right() - 3, search_y, 3, 1),
            "×",
            Action::ClearFilter,
            false,
            false,
            false,
        );
    }
    let mut section_y = search_y + if compact { 1 } else { 2 };
    section_y = current_connection(f, app, content, section_y);
    let error = app.remote_targets.error.clone();
    if error.is_some() && section_y + 3 < content.bottom() {
        line(
            f,
            content,
            section_y,
            "! Targets need attention",
            style.fg(app.colors.ui.error_text_color(bg)),
        );
        let status = if app.remote_targets.store.is_some() {
            "Showing last saved targets"
        } else {
            "Open configuration to repair it"
        };
        line(f, content, section_y + 1, status, style.fg(dim));
        app.remote_targets.hits.push((
            Rect::new(content.x, section_y, content.width, 2),
            Action::OpenConfig,
        ));
        section_y += 3;
    }
    if section_y >= content.bottom() {
        return;
    }
    let indices = app.remote_targets.indices();
    let count = app
        .remote_targets
        .store
        .as_ref()
        .map_or(0, |store| store.targets.len());
    let section = if !app.remote_targets.filter.text.is_empty() {
        format!("SSH TARGETS  {}/{count}", indices.len())
    } else {
        format!("SSH TARGETS  {count}")
    };
    line(
        f,
        content,
        section_y,
        section,
        style.fg(dim).add_modifier(Modifier::BOLD),
    );
    let footer_rows = if content.bottom().saturating_sub(section_y) >= 8 && !indices.is_empty() {
        3
    } else {
        0
    };
    let list = Rect::new(
        content.x,
        (section_y + 1).min(content.bottom()),
        content.width,
        content.bottom().saturating_sub(section_y + 1 + footer_rows),
    );
    app.remote_targets.list_area = list;
    if indices.is_empty() {
        let (title, hint) = if error.is_some() && app.remote_targets.store.is_none() {
            (
                "Cannot load saved targets.",
                "Your configuration has been kept. Open it to fix the error, then refresh.",
            )
        } else if count == 0 {
            (
                "No remote targets yet.",
                "Save an SSH host or config alias to connect again easily.",
            )
        } else {
            (
                "No matching targets.",
                "Try another name, host or folder, or clear the filter.",
            )
        };
        let top = list.y + u16::from(list.height > 3);
        let title_rows =
            App::wrap_setting_text_for_width(title, list.width.max(1) as usize).len() as u16;
        let title_height = title_rows.min(list.bottom().saturating_sub(top));
        wrapped(
            f,
            Rect::new(list.x, top, list.width, title_height),
            title,
            style,
        );
        let hint_y = top.saturating_add(title_height + 1);
        let button_y = list.bottom().saturating_sub(1);
        if button_y > hint_y {
            let hint_height = (App::wrap_setting_text_for_width(hint, list.width.max(1) as usize)
                .len() as u16)
                .min(button_y.saturating_sub(hint_y + 1));
            wrapped(
                f,
                Rect::new(list.x, hint_y, list.width, hint_height),
                hint,
                style.fg(dim),
            );
        }
        if button_y > top.saturating_add(title_height) {
            let (label, action) = if error.is_some() {
                ("Open Configuration", Action::OpenConfig)
            } else if count > 0 {
                ("Clear Filter", Action::ClearFilter)
            } else {
                ("Add SSH Target", Action::Add)
            };
            button(
                f,
                app,
                Rect::new(
                    list.x,
                    button_y,
                    list.width.min(label.width() as u16 + 4),
                    1,
                ),
                label,
                action,
                true,
                false,
                false,
            );
        }
        app.remote_targets.rows = 0;
        return;
    }
    let rows = list.height as usize / 2;
    if rows != app.remote_targets.rows {
        app.remote_targets.rows = rows;
        app.remote_targets.clamp();
    }
    app.remote_targets.scroll = app
        .remote_targets
        .scroll
        .min(indices.len().saturating_sub(rows.max(1)));
    let scrollbar = rows > 0 && indices.len() > rows && list.width >= 10;
    let row_width = list.width.saturating_sub(if scrollbar { 2 } else { 0 });
    for (position, &index) in indices
        .iter()
        .enumerate()
        .skip(app.remote_targets.scroll)
        .take(rows)
    {
        let y = list.y + ((position - app.remote_targets.scroll) * 2) as u16;
        let target = app.remote_targets.target(index).unwrap();
        let row = Rect::new(inner.x, y, (content.x - inner.x) + row_width, 2);
        let selected = position == app.remote_targets.selected;
        let hovered = row.contains((app.mouse_col, app.mouse_row).into());
        let row_style = app.colors.ui.list_row_style(
            app.colors.ui.sidebar_foreground,
            bg,
            selected,
            app.active_pane == ActivePane::Explorer && !app.remote_targets.filter_focused,
            hovered,
        );
        f.render_widget(Paragraph::new("").style(row_style), row);
        let connection = app.remote_target_connection(&target);
        let marker = match connection.map(|connection| connection.status) {
            Some(ConnectionStatus::Current | ConnectionStatus::Connected) => "●",
            Some(ConnectionStatus::Reconnecting | ConnectionStatus::SigningIn) => "↻",
            Some(ConnectionStatus::Disconnected) => "○",
            Some(ConnectionStatus::Failed) => "!",
            None => "◇",
        };
        let actions = (selected || hovered) && row_width >= 15;
        line(
            f,
            Rect::new(
                list.x,
                y,
                row_width.saturating_sub(if actions { 8 } else { 0 }),
                1,
            ),
            y,
            format!("{marker} {}", target.name),
            row_style.add_modifier(Modifier::BOLD),
        );
        let detail = if let Some(connection) = connection {
            format!("  {} · {}", target.host, connection.status.label())
        } else if !target.remote_command.trim().is_empty() {
            format!("  {} · Custom command", target.host)
        } else if !target.folder.is_empty() {
            format!("  {} · {}", target.host, target.folder)
        } else {
            format!("  {}", target.host)
        };
        let detail_fg = app
            .colors
            .ui
            .readable_foreground(dim, row_style.bg.unwrap_or(bg), 4.5);
        line(
            f,
            Rect::new(list.x, y + 1, row_width, 1),
            y + 1,
            detail,
            row_style.fg(detail_fg),
        );
        app.remote_targets
            .hits
            .push((row, Action::Select(position)));
        if actions {
            button(
                f,
                app,
                Rect::new(list.x + row_width - 7, y, 3, 1),
                "↗",
                Action::Connect(index),
                false,
                false,
                false,
            );
            button(
                f,
                app,
                Rect::new(list.x + row_width - 3, y, 3, 1),
                "⋯",
                Action::Menu(index),
                false,
                false,
                false,
            );
        }
    }
    if scrollbar {
        let (track, thumb) = app.colors.ui.scrollbar_colors(
            bg,
            list.contains((app.mouse_col, app.mouse_row).into()),
            false,
        );
        let height = rows * 2;
        let thumb_size = (height * rows / indices.len()).max(1);
        let thumb_pos = app.remote_targets.scroll * (height - thumb_size)
            / indices.len().saturating_sub(rows).max(1);
        for row in 0..height {
            let color = if row >= thumb_pos && row < thumb_pos + thumb_size {
                thumb
            } else {
                track
            };
            f.render_widget(
                Paragraph::new("▐").style(Style::default().fg(color).bg(bg)),
                Rect::new(list.right() - 1, list.y + row as u16, 1, 1),
            );
            let offset = row * indices.len().saturating_sub(rows) / height.saturating_sub(1).max(1);
            app.remote_targets.hits.push((
                Rect::new(list.right() - 1, list.y + row as u16, 1, 1),
                Action::ScrollTo(offset),
            ));
        }
    }
    if footer_rows > 0
        && let Some(index) = app.remote_targets.selected_index()
    {
        let target = app.remote_targets.target(index).unwrap();
        let label = app
            .remote_target_connection(&target)
            .map_or("Connect", |connection| connection.status.action());
        let width = (label.width() as u16 + 4).min(content.width);
        let y = content.bottom() - 3;
        button(
            f,
            app,
            Rect::new(content.x, y, width, 1),
            label,
            Action::Connect(index),
            true,
            false,
            false,
        );
        if content.width >= width + 9 {
            button(
                f,
                app,
                Rect::new(content.x + width + 1, y, 8, 1),
                "Edit",
                Action::Edit(index),
                false,
                false,
                false,
            );
        }
        line(
            f,
            content,
            content.bottom() - 1,
            if content.width >= 32 {
                "Enter Open · F2 Edit · N Add"
            } else {
                "Enter Open · F2 Edit"
            },
            style.fg(dim),
        );
    }
}
