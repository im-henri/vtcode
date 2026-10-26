//! Shared control styling for the chat toolbar, composer, and inline cards.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum ButtonKind {
    Quiet,
    Secondary,
    Toolbar,
    Primary,
    Dropdown,
}

pub(super) fn toolbar_kind(action: &ChatAction) -> ButtonKind {
    match action {
        ChatAction::Hide
        | ChatAction::New
        | ChatAction::Picker(Picker::Agents | Picker::Sessions)
        | ChatAction::DismissPicker
        | ChatAction::CloseTab(_)
        | ChatAction::CloseAgent(_)
        | ChatAction::RemoveQueued(_)
        | ChatAction::TabPage(_)
        | ChatAction::AgentPage(_)
        | ChatAction::QueuePage(_) => ButtonKind::Quiet,
        _ => ButtonKind::Toolbar,
    }
}

pub(super) fn draw_button(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    label: &str,
    action: ChatAction,
    enabled: bool,
    kind: ButtonKind,
) {
    let area = area.intersection(f.area());
    if area.is_empty() {
        return;
    }
    let c = &app.colors;
    let focused = enabled && app.chat.focus.as_ref() == Some(&action);
    let open = enabled
        && (matches!(&action, ChatAction::Picker(picker) if app.chat.picker.as_ref() == Some(picker))
            || (action == ChatAction::Yolo && app.chat.yolo_enabled()));
    let hover = enabled
        && area.contains((app.mouse_col, app.mouse_row).into())
        && (app.chat.picker.is_none()
            || matches!(
                action,
                ChatAction::DismissPicker | ChatAction::DeleteSession(_)
            ));
    let active = hover || focused || open;
    let (fg, bg) = f
        .buffer_mut()
        .cell((area.x, area.y))
        .map_or((c.ui.fg_sidebar, c.bg_sidebar), |cell| (cell.fg, cell.bg));
    let mut style = match kind {
        ButtonKind::Primary | ButtonKind::Secondary => crate::ui_widgets::action_button_style(
            c,
            bg,
            matches!(kind, ButtonKind::Primary),
            active,
            enabled,
        ),
        ButtonKind::Dropdown => {
            let control = c.ui.dropdown_colors;
            let background = control.background.over(bg).unwrap_or(bg);
            let mut style =
                c.ui.toolbar_layer_style(control.foreground, background, hover, focused, open);
            // The closed selector owns its dropdown border, including an
            // explicitly transparent one. Hover/focus outlines take priority
            // while active, so HC selectors remain visibly interactive.
            if !active {
                style = style.remove_modifier(Modifier::UNDERLINED);
                style.underline_color = None;
            }
            if style.underline_color.is_none()
                && let Some(border) = control.border.border(style.bg.unwrap_or(background)) {
                style = style
                    .add_modifier(Modifier::UNDERLINED)
                    .underline_color(border);
            }
            style
        }
        ButtonKind::Toolbar | ButtonKind::Quiet => c.ui.toolbar_style(fg, bg, hover, focused, open),
    };
    if !enabled && !matches!(kind, ButtonKind::Primary | ButtonKind::Secondary) {
        let bg = style.bg.unwrap_or(bg);
        style = style.fg(disabled_text_color(
            c.ui.disabled_foreground.over(bg).unwrap_or(fg),
            style.fg.unwrap_or(fg),
            bg,
            c.ui.contrast_colors(),
        ));
    }
    style = style.remove_modifier(Modifier::BOLD);
    if enabled && matches!(kind, ButtonKind::Primary) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if enabled && focused {
        let bg = style.bg.unwrap_or(bg);
        style = style.add_modifier(Modifier::UNDERLINED);
        if let Some(border) = c.ui.focus_border.border(bg) {
            style = style.underline_color(border);
        }
    }
    if enabled && matches!(action, ChatAction::Stop | ChatAction::StopTerminal(_)) {
        style = style.fg(c.ui.readable_foreground(c.fg_error, style.bg.unwrap_or(bg), 4.5));
    }
    if enabled && action == ChatAction::Yolo && app.chat.yolo_enabled() {
        style = style.fg(c.ui.readable_foreground(c.fg_warning, style.bg.unwrap_or(bg), 4.5));
    }
    f.render_widget(
        Paragraph::new(clean(label)).style(style).alignment(
            if matches!(kind, ButtonKind::Primary) {
                ratatui::layout::Alignment::Center
            } else {
                ratatui::layout::Alignment::Left
            },
        ),
        area,
    );
    if enabled {
        app.chat.hits.push(ChatHit { area, action });
    }
}

/// Wrap the explanation so the risk stays visible in narrow chat panes.
pub(super) fn draw_yolo_tooltip(f: &mut Frame, app: &App) {
    if app.menu_active
        || app.context_menu_active
        || app.chat.picker.is_some()
        || app.chat.files.popup.is_some()
        || crate::ui_modal::active(app).is_some()
    {
        return;
    }
    let Some(anchor) = app
        .chat
        .hits
        .iter()
        .find(|hit| {
            hit.action == ChatAction::Yolo
                && hit.area.contains((app.mouse_col, app.mouse_row).into())
        })
        .map(|hit| hit.area)
    else {
        return;
    };
    let c = &app.colors;
    let width = f.area().width.min(64).saturating_sub(4).max(1) as usize;
    let mut rows = Vec::new();
    let mut layers = Vec::new();
    for (text, color) in [
        (
            "Automatically allows all tool requests once in this chat.",
            c.ui.hover_colors.foreground,
        ),
        (
            "Can run dangerous commands and overwrite or delete files.",
            c.ui.warning_foreground,
        ),
        (
            "Off: new requests ask again. Resets on reconnect.",
            c.ui.description_foreground,
        ),
    ] {
        let first = rows.len();
        let mut line = String::new();
        for word in text.split_whitespace() {
            if !line.is_empty() && line.width() + 1 + word.width() > width {
                wrapped(&mut rows, &line, width, Color::Reset, None);
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        wrapped(&mut rows, &line, width, Color::Reset, None);
        layers.extend((first..rows.len()).map(|index| (index, color)));
    }
    let mut tooltip = crate::ui_widgets::Tooltip::hover(
        rows.into_iter().map(|row| row.line).collect(),
        c,
    )
    .title(
        if app.chat.yolo_enabled() {
            "YOLO: On"
        } else {
            "YOLO: Off"
        },
        Style::default()
            .add_modifier(Modifier::BOLD),
    )
    .prefer_below();
    for (index, foreground) in layers {
        tooltip = tooltip.line_colors(index, crate::theme_workbench::ControlColors { foreground, ..Default::default() });
    }
    tooltip.render(f, anchor.x, anchor.y);
}
