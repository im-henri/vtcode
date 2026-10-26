//! Agent and conversation tabs, followed by a restrained contextual toolbar.
use super::*;

pub(super) struct HeaderLayout {
    pub height: u16,
    pub controls: Vec<(Rect, String, ChatAction)>,
}

pub(super) fn layout(app: &App, body: Rect) -> HeaderLayout {
    let conversations = app.chat.layout.rendering.is_some()
        || app.chat.connected()
        || app
            .chat
            .open_tabs
            .iter()
            .any(|id| app.chat.sessions.contains_key(id));
    let height = if conversations { 3 } else { 2 };
    let mut bounds = Rect::new(
        body.x + 1,
        body.y + height - 1,
        body.width.saturating_sub(2),
        1,
    );
    let mut controls = Vec::new();
    // Approval policy is always legible and keeps its position as edits arrive.
    if app.chat.initialized.is_some() && app.chat.session().is_some() {
        let label = format!(
            " YOLO [{}] ",
            if app.chat.yolo_enabled() { "x" } else { " " }
        );
        let width = (label.width() as u16).min(bounds.width);
        controls.push((
            Rect::new(bounds.right() - width, bounds.y, width, 1),
            label,
            ChatAction::Yolo,
        ));
        bounds.width = bounds.width.saturating_sub(width + 1);
    }
    let view = if bounds.width >= 11 && (bounds.width >= 26 || !app.chat.rollback.enabled) {
        " Display ▾ "
    } else {
        " View ▾ "
    };
    let view_width = view.width() as u16;
    if bounds.width >= view_width {
        controls.push((
            Rect::new(bounds.right() - view_width, bounds.y, view_width, 1),
            view.into(),
            ChatAction::Picker(Picker::Display),
        ));
        bounds.width = bounds.width.saturating_sub(view_width + 1);
    }
    let pending: usize = app.chat.session().map_or(0, |s| {
        s.rollback.edits.iter().map(|edit| edit.pending()).sum()
    });
    if app.chat.rollback.enabled && pending > 0 {
        let label = format!(" Changes ({pending}) ");
        let width = label.width() as u16;
        if bounds.width >= width {
            controls.push((
                Rect::new(bounds.x, bounds.y, width, 1),
                label,
                ChatAction::Picker(Picker::Changes),
            ));
        }
    }
    controls.sort_by_key(|(rect, ..)| rect.x);
    HeaderLayout { height, controls }
}

pub(super) fn draw(f: &mut Frame, app: &mut App, body: Rect) -> u16 {
    let layout = layout(app, body);
    f.render_widget(
        Block::default().style(Style::default().fg(app.colors.ui.fg_sidebar_title)),
        Rect::new(body.x, body.y, body.width, 1),
    );
    let embedded = app.chat.layout.rendering.is_some();
    let history = if body.width >= 38 {
        " History "
    } else {
        " ◷ "
    };
    let reserved = (history.width() as u16 + 4).min(body.width);
    let available = body.width.saturating_sub(reserved + 2);
    if !embedded {
        agents::draw(f, app, Rect::new(body.x, body.y, available, 1));
    } else {
        let new_width = if app.chat.connected() { 4 } else { 0 };
        let title_width = available
            .saturating_sub(new_width)
            .min(app.chat.name().width().saturating_add(6) as u16);
        let title = format!(
            " ◇ {} ▾",
            elide(app.chat.name(), title_width.saturating_sub(6) as usize)
        );
        button(
            f,
            app,
            Rect::new(body.x, body.y, title_width, 1),
            &title,
            ChatAction::Picker(Picker::Agents),
            true,
            false,
        );
        if app.chat.connected() {
            let mut x = body.x + title_width + u16::from(title_width > 0);
            small_button(
                f,
                app,
                &mut x,
                Rect::new(body.x, body.y, available, 1),
                " + ",
                ChatAction::New,
                !app.chat.control_busy,
            );
        }
    }
    let bounds = Rect::new(
        body.right().saturating_sub(reserved + 1),
        body.y,
        reserved,
        1,
    );
    let mut x = bounds.x;
    small_button(
        f,
        app,
        &mut x,
        bounds,
        history,
        ChatAction::Picker(Picker::Sessions),
        !app.chat.control_busy,
    );
    small_button(f, app, &mut x, bounds, " × ", ChatAction::Hide, true);
    if body.height < layout.height {
        return layout.height;
    }
    if embedded {
        button(
            f,
            app,
            Rect::new(
                body.x + 1,
                body.y + 1,
                body.width.saturating_sub(2).min(25),
                1,
            ),
            " ↗ Move to Agent Chat ",
            ChatAction::MoveToDock,
            true,
            false,
        );
    } else if layout.height == 3 {
        tabs::draw(f, app, Rect::new(body.x, body.y + 1, body.width, 1));
    }
    for (rect, label, action) in layout.controls {
        if rect.bottom() <= body.bottom() {
            let enabled = action != ChatAction::Yolo || !app.chat.closing;
            widgets::draw_button(
                f,
                app,
                rect,
                &label,
                action,
                enabled,
                widgets::ButtonKind::Toolbar,
            );
        }
    }
    layout.height
}

pub(super) fn tooltip(f: &mut Frame, app: &App) {
    if app.chat.picker.is_some()
        || app.chat.files.popup.is_some()
        || app.menu_active
        || app.context_menu_active
        || crate::ui_modal::active(app).is_some()
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
    let (title, description) = match hit.action {
        ChatAction::Picker(Picker::Display) => (
            "Chat display",
            "Choose tool output detail and default thinking visibility.",
        ),
        ChatAction::Picker(Picker::Changes) => (
            "Agent changes",
            "Review pending file changes. Accept or reject by file or chunk.",
        ),
        ChatAction::Picker(Picker::Sessions) => (
            "Conversation history",
            "Reopen a saved conversation for this agent.",
        ),
        ChatAction::New => (
            "New conversation",
            "Start another conversation. Existing chats keep running.",
        ),
        ChatAction::MoveToDock => (
            "Move to Agent Chat",
            "Return this conversation to the chat dock.",
        ),
        _ => return,
    };
    crate::ui_widgets::Tooltip::hover(vec![Line::raw(description)], &app.colors)
        .title(title, Style::default().add_modifier(Modifier::BOLD))
        .prefer_below()
        .render(f, hit.area.x, hit.area.y);
}

#[cfg(test)]
#[path = "ui_chat_header_tests.rs"]
mod tests;
