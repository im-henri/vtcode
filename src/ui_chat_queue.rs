//! A compact outbox above the composer. Clicking its text restores that draft.
use super::*;

pub(super) fn height(app: &App) -> u16 {
    app.chat
        .selected
        .as_ref()
        .and_then(|id| app.chat.queues.get(id))
        .filter(|q| !q.is_empty())
        .map_or(0, |q| {
            1 + q.items.len().min(3) as u16 + u16::from(q.inflight.is_some())
        })
}

pub(super) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let Some(queue) = app
        .chat.connection
        .selected
        .as_ref()
        .and_then(|id| app.chat.connection.queues.get_mut(id))
    else {
        return;
    };
    let capacity = area
        .height
        .saturating_sub(1 + u16::from(queue.inflight.is_some())) as usize;
    queue.visible_rows = capacity;
    queue.scroll = queue.scroll.min(queue.items.len().saturating_sub(capacity));
    let items: Vec<_> = queue
        .items
        .iter()
        .skip(queue.scroll)
        .take(capacity)
        .cloned()
        .collect();
    let (count, paused, preparing) = (queue.items.len(), queue.paused, queue.inflight.is_some());
    let header = Rect::new(area.x, area.y, area.width, 1);
    let mut x = header.x;
    let label = format!("Queued ({count}){}", if paused { " · Paused" } else { "" });
    // small_button also reserves one column between adjacent controls.
    let controls_width = (if paused { 8 } else { 7 }) + if count > capacity { 8 } else { 0 };
    let label_width = (label.width() as u16).min(area.width.saturating_sub(controls_width));
    f.render_widget(
        Paragraph::new(elide(&label, label_width as usize))
            .style(Style::default().fg(app.colors.fg_dim)),
        Rect::new(x, area.y, label_width, 1),
    );
    x += label_width;
    small_button(
        f,
        app,
        &mut x,
        header,
        if paused { " Resume " } else { " Pause " },
        ChatAction::PauseQueue,
        count > 0,
    );
    if count > capacity {
        small_button(
            f,
            app,
            &mut x,
            header,
            " ‹ ",
            ChatAction::QueuePage(true),
            true,
        );
        small_button(
            f,
            app,
            &mut x,
            header,
            " › ",
            ChatAction::QueuePage(false),
            true,
        );
    }
    let mut y = area.y + 1;
    if preparing && y < area.bottom() {
        f.render_widget(
            Paragraph::new("Creating message checkpoint…")
                .style(Style::default().fg(app.colors.fg_dim)),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }
    for prompt in items {
        let row = Rect::new(area.x, y, area.width, 1);
        let preview_width = area.width.saturating_sub(15);
        let preview = if prompt.text.trim().is_empty() {
            format!("{} attached files", prompt.files.len())
        } else {
            prompt.text.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        button(
            f,
            app,
            Rect::new(area.x, y, preview_width, 1),
            &format!(
                "✎ {}",
                elide(&preview, preview_width.saturating_sub(2) as usize)
            ),
            ChatAction::EditQueued(prompt.id),
            true,
            false,
        );
        let mut x = area.x + preview_width;
        small_button(
            f,
            app,
            &mut x,
            row,
            " Send now ",
            ChatAction::SendQueued(prompt.id),
            app.chat.connected(),
        );
        small_button(
            f,
            app,
            &mut x,
            row,
            " × ",
            ChatAction::RemoveQueued(prompt.id),
            true,
        );
        y += 1;
    }
}
