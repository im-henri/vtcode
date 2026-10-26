//! Compact Item/Value table shared by the file-type and Settings entry points.
use super::*;
use ratatui::{
    layout::Margin,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear},
};
use unicode_width::UnicodeWidthStr;

struct Columns {
    item: Rect,
    value: Rect,
    edit: Rect,
    remove: Rect,
}

fn columns(area: Rect) -> Columns {
    let actions = if area.width >= 52 {
        18
    } else if area.width >= 36 {
        14
    } else {
        6
    };
    let text = area.width.saturating_sub(actions + 2);
    let item = Rect::new(area.x, area.y, text / 2, area.height);
    let value = Rect::new(
        item.right().saturating_add(1).min(area.right()),
        area.y,
        text - text / 2,
        area.height,
    );
    let action_x = value.right().saturating_add(1).min(area.right());
    let remove_width = if actions >= 14 { 8 } else { 3 };
    let edit = Rect::new(
        action_x,
        area.y,
        (actions - remove_width).min(area.right().saturating_sub(action_x)),
        area.height,
    );
    let remove = Rect::new(
        edit.right(),
        area.y,
        remove_width.min(area.right().saturating_sub(edit.right())),
        area.height,
    );
    Columns {
        item,
        value,
        edit,
        remove,
    }
}

fn label(f: &mut Frame, text: &str, area: Rect, style: Style) {
    f.render_widget(
        Paragraph::new(crate::ui_pickers::truncate_to_width(
            text,
            area.width as usize,
        ))
        .style(style),
        area,
    );
}

fn field(f: &mut Frame, app: &mut App, area: Rect, index: usize, bg: Color) {
    if area.is_empty() {
        return;
    }
    let state = &app.file_associations;
    let field = &state.fields[index];
    let focused = state.focus == Action::Field(index);
    let style = crate::ui_settings::settings_control_style(&app.colors, "text", bg, focused);
    let arrow_width = if index == 1 && area.width > 2 { 2 } else { 0 };
    let input = Rect::new(area.x, area.y, area.width - arrow_width, area.height);
    let hit = crate::ui_widgets::InputField::new(&field.text)
        .theme_control(&app.colors, app.colors.ui.settings_control("text"), bg)
        .bg(style.bg.unwrap_or(bg))
        .prefix("", Color::Reset)
        .placeholder_text(if index == 0 {
            "*.cps or filename"
        } else {
            "Search languages…"
        })
        .cursor_marker(if focused && app.cursor_visible {
            "│"
        } else {
            ""
        })
        .cursor_pos(field.cursor)
        .selection_range(if focused {
            App::inline_selection_range(field.cursor, field.anchor)
        } else {
            None
        })
        .render(f, input);
    if arrow_width > 0 {
        f.render_widget(
            Paragraph::new("⌄ ").style(style),
            Rect::new(input.right(), area.y, arrow_width, area.height),
        );
    }
    crate::ui_settings::underline_field(f, area, style);
    app.register_text_input(TextInputId::FileAssociation(index), hit);
    app.file_associations
        .hits
        .push((area, Action::Field(index)));
}

fn draw_row(f: &mut Frame, app: &mut App, row: &Row, area: Rect, bg: Color) {
    let cols = columns(area);
    let value = Rect::new(
        cols.value.x,
        area.y,
        area.right().saturating_sub(cols.value.x),
        1,
    );
    let normal = Style::default()
        .fg(app.colors.ui.widget_text_color(bg))
        .bg(bg);
    match row {
        Row::Rule(index) => {
            let state = &app.file_associations;
            let (pattern, language, local) = &state.rules[*index];
            let focused = matches!(state.focus, Action::Rule(i) | Action::Delete(i) if i == *index);
            let hovered = !state.editing && area.contains((app.mouse_col, app.mouse_row).into());
            let style = app.colors.ui.list_row_style(
                app.colors.ui.widget_foreground,
                bg,
                focused && !state.editing,
                true,
                hovered,
            );
            f.render_widget(Paragraph::new("").style(style), area);
            label(f, pattern, cols.item, style);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(language.as_str()),
                    Span::styled(
                        if *local { "" } else { " (User)" },
                        Style::default()
                            .fg(app.colors.ui.description_color(style.bg.unwrap_or(bg))),
                    ),
                ]))
                .style(style),
                cols.value,
            );
            let local = *local;
            if !state.editing {
                app.file_associations.hits.extend([
                    (cols.item, Action::Rule(*index)),
                    (cols.value, Action::Rule(*index)),
                ]);
                button(
                    f,
                    app,
                    cols.edit,
                    if cols.edit.width < 6 {
                        "✎"
                    } else if !local && cols.edit.width >= 10 {
                        "Override"
                    } else {
                        "Edit"
                    },
                    Action::Rule(*index),
                );
                if local {
                    button(
                        f,
                        app,
                        cols.remove,
                        if cols.remove.width < 8 {
                            "×"
                        } else {
                            "Remove"
                        },
                        Action::Delete(*index),
                    );
                }
            }
        }
        Row::Edit => {
            field(f, app, cols.item, 0, bg);
            field(f, app, value, 1, bg);
        }
        Row::Suggestions => {
            label(
                f,
                "Use:",
                Rect::new(area.x, area.y, area.width.min(5), 1),
                normal.fg(app.colors.ui.description_color(bg)),
            );
            let count = app.file_associations.suggestions.len();
            let width = area.width.saturating_sub(5) / count.max(1) as u16;
            for index in 0..count {
                let rect = Rect::new(
                    area.x + area.width.min(5) + index as u16 * width,
                    area.y,
                    width.saturating_sub(1),
                    1,
                );
                let text = crate::ui_pickers::truncate_to_width(
                    &app.file_associations.suggestions[index],
                    rect.width.saturating_sub(2) as usize,
                );
                button(f, app, rect, &text, Action::Suggestion(index));
            }
        }
        Row::Language(index) => {
            let colors = crate::ui_widgets::settings_dropdown_colors(&app.colors);
            let selected = app.file_associations.focus == Action::Language(*index)
                || value.contains((app.mouse_col, app.mouse_row).into());
            let style = Style::default()
                .fg(if selected {
                    colors.fg_selected
                } else {
                    colors.fg
                })
                .bg(if selected {
                    colors.bg_selected
                } else {
                    colors.bg
                });
            label(
                f,
                &format!(" {}", app.file_associations.languages[*index]),
                value,
                style,
            );
            app.file_associations
                .hits
                .push((value, Action::Language(*index)));
        }
        Row::LanguageHint(text) => label(
            f,
            text,
            value,
            normal.fg(app.colors.ui.description_color(bg)),
        ),
        Row::Empty => label(
            f,
            "No associations. Use Add Item to create one.",
            area,
            normal.fg(app.colors.ui.description_color(bg)),
        ),
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    app.file_associations.hits.clear();
    app.remove_text_input_regions(|id| matches!(id, TextInputId::FileAssociation(_)));
    let viewport = f.area();
    let width = viewport
        .width
        .saturating_sub(u16::from(viewport.width > 40) * 4)
        .min(92);
    let content = rows(&app.file_associations);
    let description = App::wrap_setting_text_for_width(
        if app.file_associations.editing {
            "Save or Cancel this row before switching User / Workspace."
        } else {
            "Associate filename patterns with a language, e.g. *.cps → C++."
        },
        width.saturating_sub(4).max(1) as usize,
    );
    let description_height = if viewport.height < 12 {
        0
    } else {
        description.len().min(2) as u16
    };
    let errors = app
        .file_associations
        .error
        .as_ref()
        .map(|error| {
            App::wrap_setting_text_for_width(error, width.saturating_sub(4).max(1) as usize)
        })
        .unwrap_or_default();
    let error_height = errors.len().min(2) as u16;
    let height = (7 + description_height + error_height + content.len().clamp(1, 10) as u16).min(
        viewport
            .height
            .saturating_sub(u16::from(viewport.height > 12) * 2),
    );
    let area = Rect::new(
        viewport.x + (viewport.width - width) / 2,
        viewport.y + ((viewport.height - height) / 2).min(3),
        width,
        height,
    );
    for cell in &mut f.buffer_mut().content {
        cell.modifier.insert(Modifier::DIM);
    }
    let bg = crate::ui_widgets::widget_background(f, &app.colors, area);
    let fg = app.colors.ui.widget_text_color(bg);
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default().fg(app
                    .colors
                    .ui
                    .widget_border
                    .border(bg)
                    .unwrap_or(app.colors.border_color)),
            )
            .style(Style::default().fg(fg).bg(bg)),
        area,
    );
    if width < 12 || height < 7 {
        return;
    }
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    label(
        f,
        if inner.width < 21 {
            "Associations"
        } else {
            "File Associations"
        },
        Rect::new(inner.x, inner.y, inner.width.saturating_sub(4), 1),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    );
    button(
        f,
        app,
        Rect::new(inner.right().saturating_sub(3), inner.y, 3, 1),
        "×",
        Action::Close,
    );
    let mut x = inner.x;
    for (scope, text) in [
        (SettingsScope::Global, "User"),
        (SettingsScope::Workspace, "Workspace"),
    ] {
        let width = (UnicodeWidthStr::width(text) as u16 + 2).min(inner.right().saturating_sub(x));
        let rect = Rect::new(x, inner.y + 2, width, 1);
        let action = Action::Scope(scope);
        let mut style = app.colors.ui.input_option_style(
            fg,
            bg,
            app.file_associations.scope == scope,
            !app.file_associations.editing && rect.contains((app.mouse_col, app.mouse_row).into()),
        );
        if app.file_associations.focus == action {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        if app.file_associations.editing && app.file_associations.scope != scope {
            style = style.fg(app.colors.ui.description_color(bg));
        }
        let caption = if width < UnicodeWidthStr::width(text) as u16 + 2 {
            text.to_owned()
        } else {
            format!(" {text} ")
        };
        label(f, &caption, rect, style);
        if !app.file_associations.editing && !rect.is_empty() {
            app.file_associations.hits.push((rect, action));
        }
        x = rect.right().saturating_add(1).min(inner.right());
    }
    for (i, text) in description
        .iter()
        .take(description_height as usize)
        .enumerate()
    {
        label(
            f,
            text,
            Rect::new(inner.x, inner.y + 3 + i as u16, inner.width, 1),
            Style::default()
                .fg(app.colors.ui.description_color(bg))
                .bg(bg),
        );
    }
    let header = Rect::new(inner.x, inner.y + 3 + description_height, inner.width, 1);
    let header_bg = app.colors.ui.keybinding_table_header.over(bg).unwrap_or(bg);
    let header_style = Style::default()
        .fg(app.colors.ui.widget_text_color(header_bg))
        .bg(header_bg)
        .add_modifier(Modifier::BOLD);
    f.render_widget(Paragraph::new("").style(header_style), header);
    let cols = columns(header);
    label(
        f,
        if cols.item.width >= 23 {
            "Item (filename pattern)"
        } else {
            "Item"
        },
        cols.item,
        header_style,
    );
    label(
        f,
        if cols.value.width < 5 {
            "Type"
        } else if cols.value.width >= 16 {
            "Value (language)"
        } else {
            "Value"
        },
        cols.value,
        header_style,
    );
    let error_height = error_height.min(inner.bottom().saturating_sub(header.bottom() + 1));
    let error_y = inner.bottom().saturating_sub(1 + error_height);
    let body = Rect::new(
        inner.x,
        header.bottom(),
        inner.width,
        error_y.saturating_sub(header.bottom()),
    );
    let state = &mut app.file_associations;
    state.max_scroll = content.len().saturating_sub(body.height as usize);
    if state.reveal_focus {
        if let Some(row) = content
            .iter()
            .position(|row| row_has_focus(row, state.focus))
        {
            if row < state.scroll {
                state.scroll = row;
            }
            if row >= state.scroll + body.height as usize {
                state.scroll = (row + 1).saturating_sub(body.height as usize);
            }
            if matches!(state.focus, Action::Field(_)) && body.height > 0 {
                // Opening an editor at the end of a long table must reveal its
                // suggestions too, rather than pinning the input to the bottom.
                let end = content
                    .iter()
                    .enumerate()
                    .skip(row + 1)
                    .find(|(_, row)| matches!(row, Row::Rule(_)))
                    .map_or(content.len(), |(index, _)| index);
                let visible_end = end.min(row + body.height as usize);
                state.scroll = state
                    .scroll
                    .max(visible_end.saturating_sub(body.height as usize));
            }
        }
        state.reveal_focus = false;
    }
    state.scroll = state.scroll.min(state.max_scroll);
    let scroll = state.scroll;
    for (index, row) in content
        .iter()
        .skip(scroll)
        .take(body.height as usize)
        .enumerate()
    {
        draw_row(
            f,
            app,
            row,
            Rect::new(body.x, body.y + index as u16, body.width, 1),
            bg,
        );
    }
    for (index, error) in errors.iter().take(error_height as usize).enumerate() {
        label(
            f,
            error,
            Rect::new(inner.x, error_y + index as u16, inner.width, 1),
            Style::default()
                .fg(app.colors.ui.error_text_color(bg))
                .bg(bg),
        );
    }
    let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    if app.file_associations.editing {
        let save = Rect::new(footer.x, footer.y, footer.width.min(6), 1);
        let cancel_x = save.right().saturating_add(1).min(footer.right());
        let cancel = Rect::new(
            cancel_x,
            footer.y,
            footer.right().saturating_sub(cancel_x).min(8),
            1,
        );
        button(f, app, save, "Save", Action::Save);
        button(f, app, cancel, "Cancel", Action::Cancel);
        if app.file_associations.can_remove() && footer.right().saturating_sub(cancel.right()) >= 9
        {
            button(
                f,
                app,
                Rect::new(footer.right() - 8, footer.y, 8, 1),
                "Remove",
                Action::Remove,
            );
        }
    } else {
        let done_width = footer.width.min(6);
        let add_width = footer.width.saturating_sub(done_width + 1).min(10);
        button(
            f,
            app,
            Rect::new(footer.x, footer.y, add_width, 1),
            "Add Item",
            Action::New,
        );
        button(
            f,
            app,
            Rect::new(footer.right() - done_width, footer.y, done_width, 1),
            "Done",
            Action::Close,
        );
        if app.file_associations.max_scroll > 0 && footer.width >= 40 {
            label(
                f,
                "Scroll for more",
                Rect::new(footer.x + 12, footer.y, footer.width - 20, 1),
                Style::default()
                    .fg(app.colors.ui.description_color(bg))
                    .bg(bg),
            );
        }
    }
}
