//! The first tab row selects a transport; the second selects its conversation.
use super::*;

pub(super) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    app.chat.agent_tab_area = Some(area);
    if app.chat.agent_tab_width != area.width {
        app.chat.reveal_agent = true;
        app.chat.agent_tab_width = area.width;
    }
    if area.width < 3 {
        return;
    }
    let tabs: Vec<_> = app
        .chat
        .agent_order
        .iter()
        .filter_map(|&id| {
            let state = app.chat.agent_state(id)?;
            let (icon, color) = match state.activity() {
                "Permission needed" => ("!", app.colors.fg_warning),
                "Working…" | "Connecting / loading…" | "Closing…" => {
                    (app.chat.activity.symbol(), app.colors.fg_accent_blue)
                }
                "Queued" => ("◌", app.colors.fg_accent_blue),
                "Needs attention" => ("!", app.colors.fg_error),
                "New activity" => ("●", app.colors.fg_success),
                "Disconnected" => ("○", app.colors.fg_dim),
                _ => ("◇", app.colors.fg_dim),
            };
            Some((id, clean(state.tab_name()), icon, color))
        })
        .collect();
    let sizes: Vec<_> = tabs
        .iter()
        .map(|(_, name, ..)| (name.width() + 7).clamp(12, 27) as u16)
        .collect();
    let overflow = sizes.iter().map(|&size| size as usize).sum::<usize>()
        > area.width.saturating_sub(4) as usize;
    let controls = if overflow && area.width >= 20 { 11 } else { 3 };
    let available = area.width.saturating_sub(controls + 1);
    app.chat.agent_tab_offset = app.chat.agent_tab_offset.min(tabs.len().saturating_sub(1));
    if !overflow {
        app.chat.agent_tab_offset = 0;
    } else if app.chat.reveal_agent
        && let Some(index) = tabs
            .iter()
            .position(|(id, ..)| *id == app.chat.active_agent)
    {
        app.chat.agent_tab_offset = app.chat.agent_tab_offset.min(index);
        while app.chat.agent_tab_offset < index
            && sizes[app.chat.agent_tab_offset..=index]
                .iter()
                .map(|&size| size.min(available) as usize)
                .sum::<usize>()
                > available as usize
        {
            app.chat.agent_tab_offset += 1;
        }
    }
    app.chat.reveal_agent = false;
    let mut x = area.x;
    let mut end = app.chat.agent_tab_offset;
    for (index, (id, name, icon, color)) in tabs.iter().enumerate().skip(app.chat.agent_tab_offset)
    {
        let width = sizes[index].min(available);
        if width < 7 || x + width > area.x + available {
            break;
        }
        let tab = Rect::new(x, area.y, width, 1);
        if *icon == app.chat.activity.symbol() {
            app.chat.activity.show();
        }
        let close = Rect::new(tab.right() - 3, tab.y, 3, 1);
        let action = ChatAction::SwitchAgent(*id);
        let selected = *id == app.chat.active_agent;
        let hovered = tab.contains((app.mouse_col, app.mouse_row).into())
            || app.chat.focus.as_ref() == Some(&action);
        let (fg, bg) = if hovered {
            (app.colors.ui.fg_tab_hover, app.colors.ui.bg_tab_hover)
        } else if selected {
            (app.colors.ui.fg_tab_active, app.colors.bg_tab_active)
        } else {
            (app.colors.ui.fg_tab_inactive, app.colors.bg_tab_inactive)
        };
        let mut style = Style::default().fg(app.colors.ui.readable_foreground(fg, bg, 4.5)).bg(bg);
        if selected {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        f.render_widget(Block::default().style(style), tab);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {icon} "),
                    Style::default().fg(app.colors.ui.readable_foreground(*color, bg, 4.5)),
                ),
                Span::raw(elide(name, width.saturating_sub(6) as usize)),
            ]))
            .style(style),
            Rect::new(tab.x, tab.y, width - 3, 1),
        );
        app.chat.hits.push(ChatHit { area: tab, action });
        button(
            f,
            app,
            close,
            if selected || hovered || app.chat.focus.as_ref() == Some(&ChatAction::CloseAgent(*id))
            {
                " × "
            } else {
                "   "
            },
            ChatAction::CloseAgent(*id),
            !app.chat.agent_state(*id).is_some_and(|state| state.closing),
            false,
        );
        x += width;
        end = index + 1;
    }
    small_button(
        f,
        app,
        &mut x,
        area,
        " + ",
        ChatAction::Picker(Picker::Agents),
        true,
    );
    if controls == 11 {
        let mut x = area.right() - 7;
        small_button(
            f,
            app,
            &mut x,
            area,
            " ‹ ",
            ChatAction::AgentPage(true),
            app.chat.agent_tab_offset > 0,
        );
        small_button(
            f,
            app,
            &mut x,
            area,
            " › ",
            ChatAction::AgentPage(false),
            end < tabs.len(),
        );
    }
}

pub(super) fn draw_tooltip(f: &mut Frame, app: &App, body: Rect) {
    if app.chat.picker.is_some()
        || app.chat.files.popup.is_some()
        || crate::ui_modal::active(app).is_some()
        || body.height < 7
    {
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
    let (title, detail) = match &hit.action {
        ChatAction::SwitchAgent(id) => {
            let Some(state) = app.chat.agent_state(*id) else {
                return;
            };
            (
                clean(state.tab_name()),
                format!(
                    "{} · {} conversations",
                    state.activity(),
                    state.sessions.len()
                ),
            )
        }
        ChatAction::Picker(Picker::Agents) => (
            "Open an agent".into(),
            "Other agents keep running in their tabs.".into(),
        ),
        ChatAction::CloseAgent(id) => {
            let Some(state) = app.chat.agent_state(*id) else {
                return;
            };
            (
                format!("Close {}", clean(state.tab_name())),
                "Close this agent and its conversations.".into(),
            )
        }
        ChatAction::New => (
            "New conversation".into(),
            "Start a conversation with this agent.".into(),
        ),
        _ => return,
    };
    let width = (title.width().max(detail.width()) + 4).min(body.width as usize) as u16;
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
            Line::raw(elide(&detail, width.saturating_sub(2) as usize)),
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
