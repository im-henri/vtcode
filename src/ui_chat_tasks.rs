//! A persistent task dropdown above the composer, independent of chat scroll.
use super::*;
use vtcode::acp::{TaskList, TaskStatus};

pub(super) fn marker(status: TaskStatus, c: &ThemeColors) -> (&'static str, Color) {
    match status {
        TaskStatus::Pending => ("○", c.fg_dim),
        TaskStatus::InProgress => ("●", c.fg_accent_blue),
        TaskStatus::Completed => ("✓", c.fg_success),
        TaskStatus::Cancelled => ("×", c.fg_dim),
        TaskStatus::Unknown => ("?", c.fg_dim),
    }
}

pub(super) fn height(app: &App) -> u16 {
    u16::from(
        app.chat
            .session()
            .and_then(|s| s.tasks.as_ref())
            .is_some_and(|tasks| !tasks.entries.is_empty()),
    )
}

pub(super) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }
    let Some(tasks) = app.chat.session().and_then(|s| s.tasks.as_ref()) else {
        return;
    };
    if tasks.entries.is_empty() {
        return;
    }
    let current = &tasks.entries[tasks.current()];
    let (icon, _) = marker(current.status, &app.colors);
    let chevron = if app.chat.picker == Some(Picker::Tasks) {
        "▾"
    } else {
        "▸"
    };
    let summary = format!(
        " {chevron} TODOs {}/{}",
        tasks.completed(),
        tasks.entries.len()
    );
    let label = if matches!(current.status, TaskStatus::Pending | TaskStatus::InProgress)
        && area.width as usize > summary.width() + 8
    {
        format!(
            "{summary} · {icon} {}",
            current.content.lines().next().unwrap_or_default()
        )
    } else {
        summary
    };
    widgets::draw_button(
        f,
        app,
        area,
        &elide(&label, area.width as usize),
        ChatAction::Picker(Picker::Tasks),
        true,
        widgets::ButtonKind::Toolbar,
    );
}

pub(super) fn rows(
    rows: &mut Vec<ChatRow>,
    tasks: &TaskList,
    width: usize,
    c: &ThemeColors,
    limit: usize,
) {
    let count = tasks.entries.len().min(limit);
    for entry in tasks.entries.iter().take(count) {
        let (icon, color) = marker(entry.status, c);
        wrapped(
            rows,
            &format!(" {icon} {}", entry.content),
            width,
            color,
            None,
        );
    }
    if count < tasks.entries.len() {
        plain(
            rows,
            format!("  … {} more tasks", tasks.entries.len() - count),
            c.fg_dim,
            None,
        );
    }
}
