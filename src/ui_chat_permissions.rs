//! Permission summaries come from the merged tool call, not just its title.
use super::*;

fn choice_layout(
    request: &p::RequestPermissionRequest,
    width: u16,
) -> Vec<(Rect, String, Option<p::PermissionOptionId>)> {
    let mut rows = Vec::new();
    let (mut x, mut y) = (0, 0);
    let choices = request
        .options
        .iter()
        .map(|o| (clean(&o.name), Some(o.option_id.clone())))
        .chain(std::iter::once(("Cancel request".into(), None)));
    for (name, id) in choices {
        let button_width = name.width().saturating_add(2).min(width as usize) as u16;
        if x > 0 && x + button_width > width {
            x = 0;
            y += 2;
        }
        rows.push((Rect::new(x, y, button_width, 1), name, id));
        x += button_width + 1;
    }
    rows
}

pub(super) fn height(app: &App, width: u16) -> u16 {
    app.chat.permission().map_or(0, |permission| {
        choice_layout(&permission.request, width.saturating_sub(4))
            .last()
            .map_or(0, |(rect, ..)| rect.bottom().saturating_add(8))
    })
}

pub(super) fn request_summary(app: &App) -> (String, String) {
    let Some(permission) = app.chat.permission() else {
        return (String::new(), String::new());
    };
    let tool = app.chat.session().and_then(|session| {
        session.entries.iter().find_map(|entry| match entry {
            ChatEntry::Tool(tool)
                if tool.tool_call_id == permission.request.tool_call.tool_call_id =>
            {
                Some(tool)
            }
            _ => None,
        })
    });
    let name = permission
        .tool_title
        .as_deref()
        .or_else(|| tool.map(|t| t.title.as_str()))
        .or(permission.request.tool_call.fields.title.as_deref())
        .unwrap_or("Unidentified tool");
    let kind = tool
        .map(|t| &t.kind)
        .or(permission.request.tool_call.fields.kind.as_ref());
    let operation = match kind {
        Some(p::ToolKind::Search) => "Search files",
        Some(p::ToolKind::Read) => "Read file",
        Some(p::ToolKind::Edit) => "Edit file",
        Some(p::ToolKind::Execute) => "Run command",
        _ => match name.trim().to_ascii_lowercase().as_str() {
            "grep" | "rg" | "ripgrep" | "search" => "Search files",
            _ => "Tool",
        },
    };
    let title = format!("{operation} · {}", clean(name));
    let mut parts = Vec::new();
    if let Some(input) = tool.and_then(|tool| tool.raw_input.as_ref()).or(permission
        .request
        .tool_call
        .fields
        .raw_input
        .as_ref())
    {
        let text = tools::readable(input);
        if !text.trim().is_empty() {
            parts.push(text);
        }
    }
    if let Some(tool) = tool {
        for location in &tool.locations {
            parts.push(app.display_path(&location.path));
        }
        for content in &tool.content {
            if let p::ToolCallContent::Content(content) = content {
                let text = content_text(&content.content);
                if !text.trim().is_empty() {
                    parts.push(text);
                }
            }
        }
    }
    if let Some(request_title) = permission.request.tool_call.fields.title.as_deref()
        && request_title != name
        && !parts.iter().any(|part| part.contains(request_title))
    {
        parts.insert(0, format!("Request: {request_title}"));
    }
    let detail = if parts.is_empty() {
        "The agent has not provided operation details.".into()
    } else {
        clean_terminal(&parts.join("\n"))
    };
    (title, detail)
}

pub(super) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(permission) = app.chat.permission() else {
        return;
    };
    let id = permission.id.clone();
    let ticket = permission.ticket;
    let request = permission.request.clone();
    let (title, detail) = request_summary(app);
    let background = app.colors.ui.widget_background_color(app.colors.bg_sidebar);
    let foreground = app.colors.ui.widget_text_color(background);
    let warning = app.colors.ui.warning_text_color(background);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(warning))
            .title(" Permission needed ")
            .style(
                Style::default()
                    .fg(foreground)
                    .bg(background),
            ),
        area,
    );
    let inside = area.inner(Margin::new(2, 1));
    if inside.height == 0 || inside.width == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new(elide(&title, inside.width as usize)).style(
            Style::default()
                .fg(warning)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(inside.x, inside.y, inside.width, 1),
    );
    let choices = choice_layout(&request, inside.width);
    let rows = choices.last().map_or(0, |(rect, ..)| rect.bottom());
    let full_choices = inside.height >= rows + 3;
    let choices_height = if full_choices { rows } else { 1 };
    let detail_height = inside.height.saturating_sub(choices_height + 2);
    f.render_widget(
        Paragraph::new(detail).style(Style::default().fg(foreground).bg(background))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        Rect::new(inside.x, inside.y + 1, inside.width, detail_height),
    );
    if inside.height > choices_height + 1 {
        button(
            f,
            app,
            Rect::new(inside.x, inside.y + 1 + detail_height, inside.width, 1),
            " View full request details ↗ ",
            ChatAction::PermissionDetails,
            true,
            false,
        );
    }
    let y = inside.bottom().saturating_sub(choices_height);
    if full_choices {
        for (rect, name, option_id) in choices {
            let kind = if option_id.is_some() {
                widgets::ButtonKind::Secondary
            } else {
                widgets::ButtonKind::Quiet
            };
            widgets::draw_button(
                f,
                app,
                Rect::new(inside.x + rect.x, y + rect.y, rect.width, 1),
                &format!(" {} ", elide(&name, rect.width.saturating_sub(2) as usize)),
                ChatAction::Permission(id.clone(), ticket, option_id),
                true,
                kind,
            );
        }
    } else if inside.height > 1 {
        secondary_button(
            f,
            app,
            Rect::new(inside.x, y, inside.width, 1),
            " Permissions… ",
            ChatAction::Picker(Picker::Permissions),
            true,
        );
    }
}
