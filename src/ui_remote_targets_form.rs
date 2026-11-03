//! Width-aware target form with a scrolling body and persistent actions.
use super::*;
use crate::app_remote_targets::Form;

enum Row {
    Label(String),
    Input(usize, &'static str),
    Hint(String),
    Advanced(String),
    Gap,
}

fn rows(form: &Form, width: u16) -> Vec<Row> {
    let custom = !form.fields[3].text.trim().is_empty();
    let mut rows = Vec::new();
    let fields = [
        (
            0,
            "Target name",
            "development",
            "Use this name with --remote-target from a terminal.",
        ),
        (
            1,
            "SSH host",
            "user@hostname or SSH alias",
            "Uses your SSH config, keys and agent. Password sign-in is also supported.",
        ),
        (
            2,
            "Remote folder (optional)",
            "~/project or /path/to/project",
            if custom {
                "The custom command below chooses the folder. This value is kept for the default command."
            } else {
                "Use an absolute path or ~/project. Empty uses the remote working directory."
            },
        ),
        (
            3,
            "Remote command (optional)",
            "vtcode --headless",
            "Replaces the default startup command and Folder field. Include the folder in this command.",
        ),
        (
            4,
            "SSH command (optional)",
            "ssh -e none -tt",
            "Local SSH executable and flags, for example ssh -e none -tt -p 2222.",
        ),
    ];
    for (id, label, placeholder, hint) in fields {
        if id == 3 {
            let count = [3, 4]
                .iter()
                .filter(|&&field| !form.fields[field].text.trim().is_empty())
                .count();
            let suffix = if count > 0 {
                format!(" · {count} configured")
            } else {
                String::new()
            };
            rows.push(Row::Advanced(format!(
                "{} Advanced connection options{suffix}",
                if form.advanced { "▾" } else { "▸" }
            )));
            if !form.advanced {
                break;
            }
            rows.push(Row::Gap);
        }
        rows.push(Row::Label(label.into()));
        rows.push(Row::Input(id, placeholder));
        rows.extend(
            App::wrap_setting_text_for_width(hint, width.max(1) as usize)
                .into_iter()
                .map(Row::Hint),
        );
        rows.push(Row::Gap);
    }
    rows
}

fn actions(form: &Form) -> Vec<(&'static str, Action, usize, bool)> {
    if form.conflict {
        vec![
            ("Reload Saved", Action::ReloadForm, 9, false),
            ("Save a Copy", Action::SaveCopy, 10, true),
            ("Cancel", Action::Cancel, 8, false),
        ]
    } else {
        vec![
            ("Save & Connect", Action::Save(true), 7, true),
            ("Save", Action::Save(false), 6, false),
            ("Cancel", Action::Cancel, 8, false),
        ]
    }
}

fn button_layout<'a>(
    width: u16,
    labels: &[(&'a str, Action, usize, bool)],
) -> (Vec<(Rect, Action, usize, bool, &'a str)>, u16) {
    let mut controls = Vec::new();
    let mut x = 0;
    let mut y = 0;
    for &(label, action, focus, primary) in labels {
        let size = (label.width() as u16 + 4).min(width);
        if x > 0 && x + size > width {
            x = 0;
            y += 1;
        }
        controls.push((Rect::new(x, y, size, 1), action, focus, primary, label));
        x += size + 1;
    }
    (controls, y + 1)
}

pub(super) fn preferred_height(app: &App, width: u16) -> u16 {
    let Some(form) = &app.remote_targets.form else {
        return 0;
    };
    let width = width.saturating_sub(4).max(1);
    let (_, buttons) = button_layout(width, &actions(form));
    let error = form.error.as_ref().map_or(0, |error| {
        App::wrap_setting_text_for_width(error, width as usize)
            .len()
            .min(4) as u16
            + 1
    });
    (rows(form, width).len() as u16).saturating_add(buttons + error + 4)
}

pub(super) fn draw_form(f: &mut Frame, app: &mut App, content: Rect, style: Style) {
    let Some(form) = &app.remote_targets.form else {
        return;
    };
    let focus = form.focus;
    let controls = actions(form);
    let (buttons, button_rows) = button_layout(content.width, &controls);
    let help_rows = u16::from(content.height >= button_rows + 4);
    let errors: Vec<_> = form.error.as_ref().map_or_else(Vec::new, |error| {
        App::wrap_setting_text_for_width(error, content.width.max(1) as usize)
    });
    let error_rows = (errors.len().min(4) as u16)
        .min(content.height.saturating_sub(button_rows + help_rows + 2));
    let footer =
        (button_rows + help_rows + error_rows + 1 + u16::from(error_rows > 0)).min(content.height);
    let mut body = Rect::new(
        content.x,
        content.y,
        content.width,
        content.height.saturating_sub(footer),
    );
    let mut layout = rows(form, body.width);
    if layout.len() > body.height as usize && body.width > 6 {
        body.width -= 2;
        layout = rows(form, body.width);
    }
    let maximum = layout.len().saturating_sub(body.height as usize);
    let form = app.remote_targets.form.as_mut().unwrap();
    if form.reveal {
        let position = layout.iter().position(|row| match row {
            Row::Input(id, _) => *id == focus,
            Row::Advanced(_) => focus == 5,
            _ => false,
        });
        if let Some(position) = position {
            let label = if focus < 5 {
                position.saturating_sub(1)
            } else {
                position
            };
            let hints = if focus < 5 {
                layout
                    .iter()
                    .skip(position + 1)
                    .take_while(|row| matches!(row, Row::Hint(_)))
                    .count()
                    .min(body.height.saturating_sub(2) as usize)
            } else {
                0
            };
            let end = position + 1 + hints;
            if label < form.scroll {
                form.scroll = label;
            }
            if end > form.scroll + body.height as usize {
                form.scroll = end.saturating_sub(body.height as usize);
            }
        }
    }
    form.scroll = form.scroll.min(maximum);
    form.reveal = false;
    let scroll = form.scroll;
    app.remote_targets.dialog_body = body;
    app.remote_targets.dialog_max_scroll = maximum;
    let bg = style.bg.unwrap_or(app.colors.ui.bg_widget);
    let dim = app.colors.ui.description_color(bg);
    for (offset, row) in layout
        .iter()
        .skip(scroll)
        .take(body.height as usize)
        .enumerate()
    {
        let y = body.y + offset as u16;
        match row {
            Row::Label(label) => line(f, body, y, label, style.add_modifier(Modifier::BOLD)),
            Row::Hint(hint) => line(f, body, y, hint, style.fg(dim)),
            Row::Input(id, placeholder) => {
                let area = Rect::new(body.x, y, body.width, 1);
                input(f, app, area, TextInputId::RemoteTarget(*id), placeholder);
                app.remote_targets
                    .dialog_hits
                    .push((area, Action::Focus(*id)));
            }
            Row::Advanced(label) => button(
                f,
                app,
                Rect::new(body.x, y, body.width, 1),
                label,
                Action::Advanced,
                false,
                focus == 5,
                true,
            ),
            Row::Gap => {}
        }
    }
    if maximum > 0 && body.height > 0 && body.width + 1 < content.width {
        let (track, thumb) = app.colors.ui.scrollbar_colors(
            bg,
            body.contains((app.mouse_col, app.mouse_row).into()),
            false,
        );
        crate::ui_widgets::VerticalScrollbar::new(
            content.right() - 1,
            body.y,
            body.height,
            layout.len(),
            scroll,
        )
        .symbols('█', '│')
        .colors(thumb, track, bg)
        .render(f);
    }
    let button_y = content
        .bottom()
        .saturating_sub(button_rows + help_rows)
        .max(content.y);
    if error_rows > 0 {
        let y = button_y.saturating_sub(error_rows + 1).max(body.bottom());
        for (row, error) in errors.iter().take(error_rows as usize).enumerate() {
            line(
                f,
                content,
                y + row as u16,
                error,
                style.fg(app.colors.ui.error_text_color(bg)),
            );
        }
    }
    for (relative, action, id, primary, label) in buttons {
        let y = button_y + relative.y;
        if y < content.bottom() - help_rows {
            button(
                f,
                app,
                Rect::new(content.x + relative.x, y, relative.width, 1),
                label,
                action,
                primary,
                focus == id,
                true,
            );
        }
    }
    if help_rows > 0 {
        let help = if app
            .remote_targets
            .form
            .as_ref()
            .is_some_and(|form| form.conflict)
        {
            "Reload replaces your draft · Save a Copy keeps both"
        } else if content.width < 44 {
            "Ctrl+S Save · Esc Cancel"
        } else {
            "Enter Save · Ctrl+Enter Connect · Esc Cancel"
        };
        line(f, content, content.bottom() - 1, help, style.fg(dim));
    }
}
