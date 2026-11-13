use crate::{
    app::{ActivePane, App, TabKind},
    git_amend::Buttons,
};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::Style,
    widgets::{Paragraph, Wrap},
};

pub(crate) fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }
    let colors = &app.colors;
    let bg = colors.ui.widget_background_color(colors.bg_editor);
    let row = Rect::new(area.x, area.y, area.width, 1);
    let button_width = if area.width >= 21 {
        9
    } else {
        5.min(area.width.saturating_sub(1) / 2)
    };
    let abort = Rect::new(row.right() - button_width, row.y, button_width, 1);
    let (amend_label, amend_width) = if area.width >= 52
        && app
            .lookup_keybinding(KeyModifiers::ALT, KeyCode::Char('a'))
            .is_none()
        && !app.shortcut_starts_chord((KeyModifiers::ALT, KeyCode::Char('a')))
    {
        ("Amend (Alt+A)", 17)
    } else {
        ("Amend", button_width)
    };
    let amend = Rect::new(
        abort.x.saturating_sub(amend_width + 1).max(row.x),
        row.y,
        amend_width,
        1,
    );
    let view = app.tabs[app.active_tab].editor.navigation_id;
    let Some(draft) = app.amend_commits.get_mut(&view) else {
        return;
    };
    draft.buttons = Some(Buttons { amend, abort });

    let busy = app.bg_git_action_running || app.bg_git_remote_running;
    let target = draft.target.as_ref();
    let detail = if draft.submitting {
        "Amending…".into()
    } else if let Some(target) = target {
        format!("{} · {}", target.label(), draft.repository)
    } else if draft.error.is_some() {
        "Could not load the commit message".into()
    } else {
        "Loading commit message…".into()
    };
    frame.render_widget(Paragraph::new("").style(Style::default().bg(bg)), row);
    let label = Rect::new(row.x, row.y, amend.x.saturating_sub(row.x + 1), 1);
    let detail = crate::ui_workspaces::clipped(&detail, label.width as usize);
    frame.render_widget(
        Paragraph::new(detail).style(Style::default().bg(bg).fg(colors.ui.description_color(bg))),
        label,
    );
    for (button, label, primary, enabled) in [
        (
            amend,
            amend_label,
            true,
            target.is_some() && !busy && !draft.submitting && app.source_control_enabled,
        ),
        (abort, "Abort", false, !draft.submitting),
    ] {
        let hovered = button.contains((app.mouse_col, app.mouse_row).into());
        if button_width >= 9 {
            crate::ui_widgets::draw_action_button(
                frame, button, label, colors, primary, hovered, enabled,
            );
        } else {
            frame.render_widget(
                Paragraph::new(label)
                    .alignment(Alignment::Center)
                    .style(
                        colors
                            .ui
                            .button_style(bg, primary, hovered && enabled, enabled),
                    ),
                button,
            );
        }
    }

    let mut body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    if body.height > 0
        && let Some(error) = &draft.error
    {
        let error = crate::ui_workspaces::escaped(error);
        let height = crate::ui_modal::wrapped_height(&error, body.width)
            .min(body.height.saturating_sub(1).max(1));
        frame.render_widget(
            Paragraph::new(error)
                .style(Style::default().bg(bg).fg(colors.ui.error_text_color(bg)))
                .wrap(Wrap { trim: true }),
            Rect::new(body.x, body.y, body.width, height),
        );
        body.y += height;
        body.height -= height;
    }
    app.viewport = Some(body);
    if body.is_empty() {
        app.code_area = None;
        app.gutter_area = None;
        app.minimap_area = None;
        app.scrollbar_area = None;
        app.h_scrollbar_area = None;
    } else {
        crate::ui_editor::draw_editor_content(frame, app, body);
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.active_pane != ActivePane::Editor
        || !app.amend_tab_is_active()
        || !matches!(key.code, KeyCode::Char('a' | 'A'))
        || key.modifiers != KeyModifiers::ALT
    {
        return false;
    }
    if key.kind == KeyEventKind::Press {
        app.submit_amend(app.tabs[app.active_tab].editor.navigation_id);
    }
    true
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent, previous: (u16, u16)) -> bool {
    if app.amend_commits.is_empty()
        || app.menu_open
        || app.context_menu_active
        || !matches!(
            mouse.kind,
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left)
        )
    {
        return false;
    }
    let hit = |x, y| {
        app.amend_commits.iter().find_map(|(&view, draft)| {
            let buttons = draft.buttons?;
            let position = (x, y).into();
            let amend = buttons.amend.contains(position);
            if !amend && !buttons.abort.contains(position) {
                return None;
            }
            app.visible_tab_group(view, TabKind::AmendCommit)
                .map(|_| (view, amend))
        })
    };
    let hovered = hit(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Moved if hovered != hit(previous.0, previous.1) => {
            app.mouse_hover_changed = true;
            true
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let Some((view, amend)) = hovered else {
                return false;
            };
            if amend {
                app.submit_amend(view);
            } else {
                app.abort_amend(view);
            }
            true
        }
        _ => false,
    }
}
