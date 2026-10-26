//! Version selection is an explicit second step of installing a release package.
use crate::{
    app::App,
    text_input::TextInputId,
    ui_widgets::{InputField, draw_dialog_button},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

fn activate(app: &mut App, action: usize) {
    match action {
        0 => app.start_extension_install(),
        1 => app.refresh_extension_versions(),
        2 => app.back_to_extension_link(),
        _ => app.close_palette(),
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.back_to_extension_link(),
        KeyCode::Tab | KeyCode::BackTab => {
            let reverse =
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            app.modal.focus = (app.modal.focus + if reverse { 4 } else { 1 }) % 5;
        }
        KeyCode::Up => app.move_extension_version_selection(-1),
        KeyCode::Down => app.move_extension_version_selection(1),
        KeyCode::PageUp => app.move_extension_version_selection(-10),
        KeyCode::PageDown => app.move_extension_version_selection(10),
        KeyCode::Enter => activate(app, app.modal.focus.saturating_sub(1)),
        KeyCode::Char(' ') if app.modal.focus > 0 => activate(app, app.modal.focus - 1),
        _ if app.modal.focus == 0
            && !app.palette.theme_install_running
            && app
                .edit_text_input(TextInputId::CommandPalette, key.code, key.modifiers, None)
                .changed =>
        {
            app.text_input_changed(TextInputId::CommandPalette);
        }
        _ => {}
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    if app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers)
        && !app.palette.theme_install_running
    {
        return;
    }
    let point = (mouse.column, mouse.row).into();
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        app.move_extension_version_selection(if mouse.kind == MouseEventKind::ScrollUp {
            -3
        } else {
            3
        });
        return;
    }
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }
    if app.modal.close.contains(point)
        || (!app.modal.area.is_empty() && !app.modal.area.contains(point))
    {
        app.close_palette();
        return;
    }
    if let Some((_, action)) = app
        .modal
        .buttons
        .iter()
        .find(|(area, _)| area.contains(point))
        .copied()
    {
        app.modal.focus = action + 1;
        activate(app, action);
        return;
    }
    if !app.palette.theme_install_running
        && let Some(picker) = &mut app.palette.extension_versions
        && picker.list_area.contains(point)
    {
        let index = picker.scroll + usize::from(mouse.row - picker.list_area.y);
        if index < picker.filtered.len() {
            picker.selected = index;
            app.modal.focus = 0;
        }
    }
}

fn clean(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let bounds = f.area();
    let width = bounds
        .width
        .saturating_sub(4)
        .clamp(1, 96)
        .min(bounds.width);
    let desired_height = app
        .palette
        .extension_versions
        .as_ref()
        .map_or(14, |picker| {
            11 + u16::from(!picker.catalog.installed.is_empty())
                + picker.catalog.versions.len().clamp(1, 12) as u16
                + 2 * u16::from(app.palette.theme_install_error.is_some())
        })
        .max(14);
    let height = bounds
        .height
        .saturating_sub(2)
        .max(1)
        .min(desired_height)
        .min(bounds.height);
    let area = Rect::new(
        bounds.x + bounds.width.saturating_sub(width) / 2,
        bounds.y + bounds.height.saturating_sub(height) / 2,
        width,
        height,
    );
    app.modal.kind = Some(crate::ui_modal::Kind::ThemeInstall);
    app.modal.area = area;
    app.modal.buttons.clear();
    app.modal.close = Rect::default();
    app.text_input_regions.clear();
    if let Some(picker) = &mut app.palette.extension_versions {
        picker.list_area = Rect::default();
    }
    let bg = app.colors.ui.bg_widget;
    let fg = app.colors.ui.workbench_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    let style = Style::default().fg(fg).bg(bg);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Choose Extension Version ")
        .border_style(Style::default().fg(app.colors.fg_accent).bg(bg))
        .style(style);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if area.width >= 5 && area.height >= 2 {
        app.modal.close = Rect::new(area.right() - 4, area.y, 3, 1);
        f.render_widget(Paragraph::new(" × ").style(style), app.modal.close);
    }
    if inner.is_empty() {
        return;
    }
    let Some(picker) = app.palette.extension_versions.as_ref() else {
        return;
    };
    let roomy = inner.height >= 12;
    let has_installed = !picker.catalog.installed.is_empty();
    let has_error = app.palette.theme_install_error.is_some();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(u16::from(roomy)),
            Constraint::Length(u16::from(roomy && has_installed)),
            Constraint::Length(u16::from(roomy)),
            Constraint::Length(if roomy { 3 } else { 1 }),
            Constraint::Min(1),
            Constraint::Length(if roomy { 2 } else { 0 }),
            Constraint::Length(if has_error {
                if roomy { 2 } else { 1 }
            } else {
                0
            }),
            Constraint::Length(1),
            Constraint::Length(u16::from(roomy)),
        ])
        .split(inner);
    f.render_widget(
        Paragraph::new(clean(&picker.catalog.source.repository)).style(style),
        rows[0],
    );
    let installed = picker
        .catalog
        .installed
        .iter()
        .map(|info| {
            format!(
                "{} {} · {} · {}",
                clean(&info.name),
                clean(&info.version),
                clean(&info.tag),
                clean(info.sha256.get(..8).unwrap_or(&info.sha256))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    f.render_widget(
        Paragraph::new(format!("Installed: {installed}")).style(style.fg(dim)),
        rows[1],
    );
    let summary = if let Some(latest) = picker.catalog.latest_stable() {
        format!("Choose a release. Latest stable: {}", latest.name)
    } else {
        "No stable release yet. Prerelease packages are available.".to_string()
    };
    f.render_widget(Paragraph::new(summary).style(style.fg(dim)), rows[2]);
    let input_style = app.colors.ui.input_style(bg);
    let input_bg = input_style.bg.unwrap_or(bg);
    let input_border = Block::default()
        .borders(if roomy { Borders::ALL } else { Borders::NONE })
        .border_style(Style::default().fg(app.colors.fg_accent).bg(input_bg))
        .style(input_style);
    let input_inner = input_border.inner(rows[3]);
    f.render_widget(input_border, rows[3]);
    if !input_inner.is_empty() {
        let field = InputField::new(&app.palette.query)
            .theme_colors(&app.colors)
            .prefix(" Filter ", fg)
            .bg(input_bg)
            .placeholder_text("Search releases…")
            .cursor_marker(
                if app.modal.focus == 0 && !app.palette.theme_install_running {
                    "│"
                } else {
                    ""
                },
            )
            .cursor_pos(app.palette.cursor)
            .selection_range(if app.modal.focus == 0 {
                App::inline_selection_range(app.palette.cursor, app.palette.selection_anchor)
            } else {
                None
            });
        let hit = field.render(f, input_inner);
        if !app.palette.theme_install_running {
            app.register_text_input(TextInputId::CommandPalette, hit);
        }
    }
    let picker = app.palette.extension_versions.as_mut().unwrap();
    picker.list_area = rows[4];
    let visible = usize::from(rows[4].height);
    picker.selected = picker.selected.min(picker.filtered.len().saturating_sub(1));
    if picker.selected < picker.scroll {
        picker.scroll = picker.selected;
    }
    if visible > 0 && picker.selected >= picker.scroll + visible {
        picker.scroll = picker.selected + 1 - visible;
    }
    picker.scroll = picker
        .scroll
        .min(picker.filtered.len().saturating_sub(visible));
    let latest = picker
        .catalog
        .latest_stable()
        .map(|version| version.name.as_str());
    let lines = picker
        .filtered
        .iter()
        .enumerate()
        .skip(picker.scroll)
        .take(visible)
        .map(|(position, &index)| {
            let version = &picker.catalog.versions[index];
            let selected = position == picker.selected;
            let background = if selected {
                app.colors.ui.bg_quick_selected
            } else {
                bg
            };
            let foreground = if selected {
                app.colors.ui.fg_quick_selected
            } else {
                fg
            };
            let foreground = app
                .colors
                .ui
                .readable_foreground(foreground, background, 4.5);
            let suffix = if version.is_installed(&picker.catalog.installed) {
                " · installed"
            } else {
                ""
            };
            let category = if latest == Some(version.name.as_str()) {
                "latest stable"
            } else {
                version.category()
            };
            let text = format!(
                "{} {}  ·  {category}{suffix}",
                if selected { "›" } else { " " },
                version.name
            );
            let clipped = crate::ui_pickers::truncate_to_width(&text, rows[4].width as usize);
            let padded = format!(
                "{}{}",
                clipped,
                " ".repeat(usize::from(rows[4].width).saturating_sub(clipped.width()))
            );
            Line::from(vec![Span::styled(
                padded,
                Style::default()
                    .fg(foreground)
                    .bg(background)
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            )])
        })
        .collect::<Vec<_>>();
    if picker.filtered.is_empty() {
        f.render_widget(
            Paragraph::new("No matching versions. Clear the filter to see all versions.")
                .style(style.fg(dim)),
            rows[4],
        );
    } else {
        f.render_widget(Paragraph::new(lines).style(style), rows[4]);
    }
    let detail = picker
        .selected_version()
        .map(|version| {
            let note = if version.category() == "prerelease" {
                "Prerelease; it may contain unfinished changes."
            } else {
                "The published package is verified before installation."
            };
            format!(
                "{} {} · {:.1} KiB · SHA-256 {}\n{note}",
                version.category(),
                version.name,
                version.size as f64 / 1024.0,
                version.sha256.get(..12).unwrap_or(&version.sha256)
            )
        })
        .unwrap_or_default();
    f.render_widget(Paragraph::new(detail).style(style.fg(dim)), rows[5]);
    let notice = app
        .palette
        .theme_install_error
        .as_deref()
        .map(clean)
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(notice)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .style(style.fg(app.colors.ui.error_text_color(bg))),
        rows[6],
    );
    let busy = app.palette.theme_install_running;
    let primary = if app.palette.extension_installing {
        "Installing…"
    } else if busy {
        "Refreshing…"
    } else {
        "Install selected"
    };
    let mut x = rows[7].x;
    for (index, label) in [primary, "Refresh", "Back", "Cancel"].iter().enumerate() {
        let width = (label.width() as u16 + 4).min(rows[7].right().saturating_sub(x));
        let rect = Rect::new(x, rows[7].y, width, rows[7].height.min(1));
        let enabled = match index {
            0 => !busy && !picker.filtered.is_empty(),
            1 => !busy,
            _ => true,
        };
        draw_dialog_button(
            f,
            rect,
            label,
            &app.colors,
            index == 0,
            app.modal.focus == index + 1,
            false,
            enabled,
        );
        if !rect.is_empty() {
            app.modal.buttons.push((rect, index));
        }
        x = x
            .saturating_add(width)
            .saturating_add(1)
            .min(rows[7].right());
    }
    f.render_widget(
        Paragraph::new("↑/↓ Select   Enter Install   Tab Actions   Esc Back").style(style.fg(dim)),
        rows[8],
    );
}
