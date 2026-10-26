//! Compact, theme-native conversation tabs within the chat dock.
use super::*;

pub(super) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    app.chat.tab_area = Some(area);
    if app.chat.tab_width != area.width {
        app.chat.reveal_tab = true;
        app.chat.tab_width = area.width;
    }
    f.render_widget(
        Block::default().style(Style::default().bg(app.colors.bg_tabbar)),
        area,
    );
    if area.width < 6 {
        return;
    }
    let tabs: Vec<_> = app
        .chat
        .open_tabs
        .iter()
        .filter_map(|id| {
            let session = app.chat.sessions.get(id)?;
            let permission = app
                .chat
                .permissions
                .iter()
                .any(|p| &p.request.session_id == id);
            let (icon, color) = if permission {
                ("!", app.colors.fg_warning)
            } else if session.busy {
                (app.chat.activity.symbol(), app.colors.fg_accent_blue)
            } else if app.chat.unread_tabs.contains(id) {
                (
                    "●",
                    if matches!(session.entries.last(), Some(ChatEntry::Error(_))) {
                        app.colors.fg_error
                    } else {
                        app.colors.fg_success
                    },
                )
            } else {
                ("◇", app.colors.fg_dim)
            };
            Some((id.clone(), clean(&session.title), icon, color))
        })
        .collect();
    let sizes: Vec<_> = tabs
        .iter()
        .map(|(_, title, _, _)| (title.width() + 7).clamp(12, 26) as u16)
        .collect();
    let new_width = if app.chat.connected() { 3 } else { 0 };
    let overflow =
        sizes.iter().map(|&w| w as usize).sum::<usize>() > area.width.saturating_sub(new_width) as usize;
    let paging = overflow && area.width >= 15;
    let controls = new_width + if paging { 8 } else { 0 };
    let available = area.width.saturating_sub(controls);
    app.chat.tab_offset = app.chat.tab_offset.min(tabs.len().saturating_sub(1));
    if !overflow {
        app.chat.tab_offset = 0;
    } else if app.chat.reveal_tab
        && let Some(selected) = tabs
            .iter()
            .position(|(id, ..)| Some(id) == app.chat.selected.as_ref())
    {
        app.chat.tab_offset = app.chat.tab_offset.min(selected);
        while app.chat.tab_offset < selected
            && sizes[app.chat.tab_offset..=selected]
                .iter()
                .map(|&w| w.min(available) as usize)
                .sum::<usize>()
                > available as usize
        {
            app.chat.tab_offset += 1;
        }
    }
    app.chat.reveal_tab = false;
    let mut x = area.x;
    let mut end = app.chat.tab_offset;
    for (index, (id, title, icon, color)) in tabs.iter().enumerate().skip(app.chat.tab_offset) {
        let width = sizes[index].min(available);
        if x + width > area.right() - controls || width < 4 {
            break;
        }
        let tab = Rect::new(x, area.y, width, 1);
        if *icon == app.chat.activity.symbol() {
            app.chat.activity.show();
        }
        let close = Rect::new(tab.right().saturating_sub(3), tab.y, 3.min(width), 1);
        let action = ChatAction::Switch(id.clone());
        let selected = app.chat.selected.as_ref() == Some(id);
        let hovered = tab.contains((app.mouse_col, app.mouse_row).into());
        let focused = app.chat.focus.as_ref() == Some(&action);
        let (fg, bg) = if hovered || focused {
            (app.colors.ui.fg_tab_hover, app.colors.ui.bg_tab_hover)
        } else if selected {
            (app.colors.ui.fg_tab_active, app.colors.bg_tab_active)
        } else {
            (app.colors.ui.fg_tab_inactive, app.colors.bg_tab_inactive)
        };
        let mut style = Style::default().fg(app.colors.ui.readable_foreground(fg, bg, 4.5)).bg(bg);
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        f.render_widget(Block::default().style(style), tab);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {icon} "),
                    Style::default().fg(app.colors.ui.readable_foreground(*color, bg, 4.5)),
                ),
                Span::raw(elide(title, width.saturating_sub(6) as usize)),
            ]))
            .style(style),
            Rect::new(tab.x, tab.y, width.saturating_sub(3), 1),
        );
        app.chat.hits.push(ChatHit { area: tab, action });
        button(
            f,
            app,
            close,
            if selected || hovered || app.chat.focus.as_ref() == Some(&ChatAction::CloseTab(id.clone())) { " × " } else { "   " },
            ChatAction::CloseTab(id.clone()),
            true,
            false,
        );
        x += width;
        end = index + 1;
    }
    if app.chat.connected() {
        small_button(f, app, &mut x, area, " + ", ChatAction::New, !app.chat.control_busy);
    }
    if paging {
        let mut x = area.right() - 7;
        small_button(f, app, &mut x, area, " ‹ ", ChatAction::TabPage(true), app.chat.tab_offset > 0);
        small_button(f, app, &mut x, area, " › ", ChatAction::TabPage(false), end < tabs.len());
    }
}

pub(super) fn draw_tooltip(f: &mut Frame, app: &App, body: Rect) {
    if app.chat.picker.is_some() || app.chat.files.popup.is_some() || body.height < 7 {
        return;
    }
    let Some(hit) = app
        .chat
        .hits
        .iter()
        .rev()
        .find(|hit| hit.area.contains((app.mouse_col, app.mouse_row).into()))
    else {
        return;
    };
    let ChatAction::Switch(id) = &hit.action else {
        return;
    };
    let Some(session) = app.chat.sessions.get(id) else {
        return;
    };
    let status = if app
        .chat
        .permissions
        .iter()
        .any(|p| &p.request.session_id == id)
    {
        "Permission needed"
    } else if session.cancelling {
        "Stopping…"
    } else if session.busy {
        "Working…"
    } else if app.chat.unread_tabs.contains(id) {
        if session.stop_reason.is_some() { "Finished · unread" } else { "New activity" }
    } else {
        "Ready"
    };
    let title = clean(&session.title);
    let width = (title.width().max(status.width()) + 4).min(body.width as usize) as u16;
    let area = Rect::new(
        hit.area.x.min(body.right().saturating_sub(width)),
        hit.area.bottom(),
        width,
        4,
    );
    let c = &app.colors;
    let surface = crate::ui_widgets::surface_background(f, area, c.bg_sidebar);
    let style = c.ui.hover_style(surface);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::styled(
                elide(&title, width.saturating_sub(2) as usize),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw(status),
        ])
        .style(style)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(c.ui.hover_colors.border.border(style.bg.unwrap()).unwrap_or(style.bg.unwrap()))),
        ),
        area,
    );
}
