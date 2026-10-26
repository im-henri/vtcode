//! A nonmodal progress card; editor input remains available during deletion.
use crate::{app::App, file_deletion::Phase};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let Some(deletion) = &mut app.file_deletion else {
        return;
    };
    deletion.area = None;
    deletion.button = None;
    if !deletion.visible() {
        return;
    }
    deletion.shown = true;
    let screen = f.area();
    if screen.width < 16 || screen.height < 4 {
        return;
    }
    let width = screen.width.saturating_sub(2).min(62);
    let height = screen.height.saturating_sub(1).min(7);
    let area = Rect::new(
        screen.right().saturating_sub(width + 1),
        screen.bottom().saturating_sub(height + 1),
        width,
        height,
    );
    let bg = app.colors.ui.bg_panel;
    let fg = app.colors.ui.workbench_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    let progress = deletion.shared.snapshot();
    let stopping = deletion.cancel.is_cancelled() && deletion.finished.is_none();
    let heading = if stopping {
        " Stopping deletion… "
    } else {
        match progress.phase {
            Phase::Queued => " Deletion queued ",
            Phase::Counting => " Preparing deletion… ",
            Phase::Deleting => " Deleting… ",
            Phase::Complete => " Deletion complete ",
            Phase::Cancelled => " Deletion cancelled ",
            Phase::Failed => " Deletion needs attention ",
        }
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(heading)
            .style(Style::default().bg(bg).fg(fg)),
        area,
    );
    let inner = Rect::new(area.x + 1, area.y + 1, width - 2, height - 2);
    let label = crate::ui_pickers::truncate_to_width(&deletion.label, inner.width as usize);
    f.render_widget(
        Paragraph::new(label).style(Style::default().fg(fg).bg(bg)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    if inner.height > 1 {
        let status = match progress.phase {
            Phase::Queued => "Waiting to start…".into(),
            Phase::Counting => format!("Counting files and folders: {} found", progress.total),
            Phase::Cancelled if !progress.total_known => {
                "No items deleted · counting cancelled".into()
            }
            _ if progress.failed > 0 => format!(
                "{} deleted · {} failed · {} total",
                progress.deleted, progress.failed, progress.total
            ),
            _ => format!(
                "{} / {} items · {}%",
                progress.processed,
                progress.total,
                if progress.total == 0 {
                    0
                } else {
                    progress.processed.saturating_mul(100) / progress.total
                }
            ),
        };
        f.render_widget(
            Paragraph::new(status).style(Style::default().fg(fg).bg(bg)),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
    if inner.height > 2 {
        let hint = if stopping {
            "Waiting for the current filesystem operation."
        } else if deletion.finished.is_some() && progress.phase == Phase::Cancelled {
            "Already deleted items cannot be restored here."
        } else if progress.phase == Phase::Complete {
            "Deletion finished."
        } else if progress.failed > 0 {
            "See Notifications for the error details."
        } else {
            "Cancel stops the remaining deletions."
        };
        f.render_widget(
            Paragraph::new(hint).style(Style::default().fg(dim).bg(bg)),
            Rect::new(inner.x, inner.y + 2, inner.width, 1),
        );
    }
    let label = if deletion.finished.is_some() {
        " Close "
    } else if stopping {
        " Stopping… "
    } else {
        " Cancel "
    };
    let button = Rect::new(
        inner.right().saturating_sub(label.chars().count() as u16),
        inner.bottom() - 1,
        (label.chars().count() as u16).min(inner.width),
        1,
    );
    // Even a four-row terminal has a distinct action row.
    f.render_widget(
        Paragraph::new(label).style(
            crate::ui_widgets::action_button_style(
                &app.colors,
                bg,
                false,
                button.contains((app.mouse_col, app.mouse_row).into()),
                !stopping,
            )
            .add_modifier(Modifier::BOLD),
        ),
        button,
    );
    deletion.area = Some(area);
    deletion.button = Some(button);
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    // Modal/palette/menu surfaces above the card keep their input ownership.
    if crate::ui_modal::active(app).is_some()
        || app.palette.active
        || app.menu_open
        || app.context_menu_active
        || app.explorer_context_active
        || app.sc_context_active
    {
        return false;
    }
    let Some(deletion) = &app.file_deletion else {
        return false;
    };
    if !deletion
        .area
        .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
    {
        return false;
    }
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && deletion
            .button
            .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
    {
        if deletion.finished.is_some() {
            app.file_deletion = None;
        } else {
            app.cancel_file_deletion();
        }
    }
    true
}
