//! Compact formatter form with an explicit scope and one provider selector.
use super::*;
use crate::ui_widgets::{DropdownItem, DropdownMenu};
use ratatui::{
    layout::Margin,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

enum Row {
    Gap,
    Hint(String),
    Language,
    Provider,
    Executable,
    Mode,
    Arguments,
    Argument(usize, usize),
    AddArgument,
    Reset,
}

fn rows(app: &App, width: u16) -> Vec<Row> {
    let state = &app.formatter_settings;
    let mut rows = vec![Row::Language, Row::Gap, Row::Provider];
    let hint = match state.provider {
        Provider::Command => {
            "Format the current document, including unsaved edits. Ctrl+Z undoes formatting."
        }
        Provider::Lsp if state.language() == "python" => {
            "Requires a server with formatting support. Pyright does not format Python."
        }
        Provider::Lsp => "Use the formatting support of this language's configured server.",
        Provider::Disabled => "Skip manual formatting and format on save for this language.",
    };
    rows.extend(
        App::wrap_setting_text_for_width(hint, width.max(1) as usize)
            .into_iter()
            .map(Row::Hint),
    );
    if state.provider == Provider::Command {
        rows.extend([Row::Gap, Row::Executable]);
        let hint = "Enter an executable or full path, without surrounding quotes.";
        rows.extend(
            App::wrap_setting_text_for_width(hint, width.max(1) as usize)
                .into_iter()
                .map(Row::Hint),
        );
        rows.extend([Row::Gap, Row::Mode]);
        let hint = match state.mode {
            CommandMode::File => "The tool rewrites {file}, a temporary copy beside the document. VTCode applies the result in the editor; save when ready.",
            CommandMode::Stdin => "VTCode sends the document to stdin and reads formatted code from stdout. Supply any flags your tool needs for this mode.",
            CommandMode::FileStdout => "The tool reads {file}, a temporary copy, and writes the formatted code to stdout.",
        };
        rows.extend(App::wrap_setting_text_for_width(hint, width.max(1) as usize).into_iter().map(Row::Hint));
        rows.extend([Row::Gap, Row::Arguments]);
        let hint = "Only the arguments below are passed. Nothing is appended automatically.";
        rows.extend(App::wrap_setting_text_for_width(hint, width.max(1) as usize).into_iter().map(Row::Hint));
        if state.arguments_open {
            let hint = "{file}: working copy of this document. {workspace}: workspace folder. One argument per row; no surrounding quotes.";
            rows.extend(
                App::wrap_setting_text_for_width(hint, width.max(1) as usize)
                    .into_iter()
                    .map(Row::Hint),
            );
            rows.extend(
                state
                    .arguments
                    .iter()
                    .enumerate()
                    .map(|(index, &id)| Row::Argument(id, index + 1)),
            );
            rows.push(Row::AddArgument);
        }
    }
    if width < 38 && state.request.is_none() && local_config(app).is_some() {
        rows.push(Row::Reset);
    }
    rows
}

fn row_has_focus(row: &Row, focus: Action) -> bool {
    match (row, focus) {
        (Row::Language, Action::Field(0) | Action::ChooseLanguage)
        | (Row::Provider, Action::ChooseProvider)
        | (Row::Executable, Action::Field(1))
        | (Row::Mode, Action::ChooseMode)
        | (Row::Arguments, Action::Arguments)
        | (Row::AddArgument, Action::AddArgument)
        | (Row::Reset, Action::Reset) => true,
        (Row::Argument(id, _), Action::Field(f) | Action::RemoveArgument(f)) => *id == f,
        _ => false,
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

fn button(f: &mut Frame, app: &mut App, area: Rect, text: &str, action: Action) {
    if area.is_empty() {
        return;
    }
    let bg = crate::ui_widgets::surface_background(f, area, app.colors.ui.bg_widget);
    let mut style = app.colors.ui.button_style(
        bg,
        action == Action::Save,
        area.contains((app.mouse_col, app.mouse_row).into()),
        true,
    );
    if app.formatter_settings.focus == action {
        style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    }
    label(f, &format!(" {text} "), area, style);
    app.formatter_settings.hits.push((area, action));
}

fn columns(area: Rect) -> (Rect, Rect) {
    let width = if area.width >= 44 {
        13
    } else if area.width >= 28 {
        11
    } else {
        8
    };
    let width = width.min(area.width);
    (
        Rect::new(area.x, area.y, width, 1),
        Rect::new(area.x + width, area.y, area.width - width, 1),
    )
}

fn field(f: &mut Frame, app: &mut App, area: Rect, id: usize, placeholder: &str, bg: Color) {
    if area.is_empty() {
        return;
    }
    let state = &app.formatter_settings;
    let input = &state.fields[id];
    let focused = state.focus == Action::Field(id);
    let style = crate::ui_settings::settings_control_style(&app.colors, "text", bg, focused);
    let hit = crate::ui_widgets::InputField::new(&input.text)
        .theme_control(&app.colors, app.colors.ui.settings_control("text"), bg)
        .bg(style.bg.unwrap_or(bg))
        .prefix("", Color::Reset)
        .placeholder_text(placeholder)
        .cursor_marker(if focused && app.cursor_visible {
            "│"
        } else {
            ""
        })
        .cursor_pos(input.cursor)
        .selection_range(if focused {
            App::inline_selection_range(input.cursor, input.anchor)
        } else {
            None
        })
        .render(f, area);
    crate::ui_settings::underline_field(f, area, style);
    app.register_text_input(TextInputId::FormatterSetting(id), hit);
    app.formatter_settings.hits.push((area, Action::Field(id)));
}

fn draw_row(f: &mut Frame, app: &mut App, row: &Row, area: Rect, bg: Color) {
    let (caption, control) = columns(area);
    let normal = Style::default()
        .fg(app.colors.ui.widget_text_color(bg))
        .bg(bg);
    let subdued = normal.fg(app.colors.ui.description_color(bg));
    match row {
        Row::Gap => {}
        Row::Hint(text) => label(f, text, area, subdued),
        Row::Language => {
            label(f, "Language", caption, normal);
            if app.formatter_settings.request.is_some() {
                label(
                    f,
                    &format!("{}  (current file)", app.formatter_settings.language()),
                    control,
                    normal.add_modifier(Modifier::BOLD),
                );
            } else {
                let input = Rect {
                    width: control.width.saturating_sub(3),
                    ..control
                };
                field(f, app, input, 0, "python, cpp, rust…", bg);
                button(
                    f,
                    app,
                    Rect::new(input.right(), area.y, control.width.min(3), 1),
                    "⌄",
                    Action::ChooseLanguage,
                );
            }
        }
        Row::Provider | Row::Mode => {
            let (name, value, action) = if matches!(row, Row::Mode) {
                ("Input/output", app.formatter_settings.mode.label(), Action::ChooseMode)
            } else {
                ("Formatter", app.formatter_settings.provider.label(), Action::ChooseProvider)
            };
            label(f, name, caption, normal);
            let style = crate::ui_settings::settings_control_style(
                &app.colors,
                "dropdown",
                bg,
                app.formatter_settings.focus == action,
            );
            label(f, "", control, style);
            label(
                f,
                &format!(" {value}"),
                Rect {
                    width: control.width.saturating_sub(2),
                    ..control
                },
                style,
            );
            if control.width >= 3 {
                label(f, "⌄ ", Rect::new(control.right() - 2, area.y, 2, 1), style);
            }
            crate::ui_settings::underline_field(f, control, style);
            app.formatter_settings
                .hits
                .push((control, action));
        }
        Row::Executable => {
            label(
                f,
                if caption.width < 11 {
                    "Command"
                } else {
                    "Executable"
                },
                caption,
                normal,
            );
            field(
                f,
                app,
                control,
                1,
                "Executable name or full path",
                bg,
            );
        }
        Row::Arguments => {
            let state = &app.formatter_settings;
            let text = format!(
                "{} Arguments{}",
                if state.arguments_open { "▾" } else { "▸" },
                if state.arguments.is_empty() {
                    " (optional)".into()
                } else {
                    format!(" ({})", state.arguments.len())
                }
            );
            let rect = Rect {
                width: area.width.min(text.chars().count() as u16 + 2),
                ..area
            };
            button(f, app, rect, &text, Action::Arguments);
        }
        Row::Argument(id, number) => {
            label(f, &format!("  {number}"), caption, subdued);
            let input = Rect {
                width: control.width.saturating_sub(4),
                ..control
            };
            field(f, app, input, *id, "Argument", bg);
            let remove_x = input.right().saturating_add(1).min(control.right());
            button(
                f,
                app,
                Rect::new(
                    remove_x,
                    area.y,
                    control.right().saturating_sub(remove_x),
                    1,
                ),
                "×",
                Action::RemoveArgument(*id),
            );
        }
        Row::AddArgument => button(
            f,
            app,
            Rect {
                width: control.width.min(16),
                ..control
            },
            "+ Add Argument",
            Action::AddArgument,
        ),
        Row::Reset => button(
            f,
            app,
            Rect {
                width: area.width.min(7),
                ..area
            },
            "Reset",
            Action::Reset,
        ),
    }
}

fn draw_menu(f: &mut Frame, app: &mut App) {
    let state = &app.formatter_settings;
    let Some(menu) = state.menu else {
        return;
    };
    let anchor_action = match menu {
        Menu::Language => Action::Field(0),
        Menu::Provider => Action::ChooseProvider,
        Menu::Mode => Action::ChooseMode,
    };
    let Some(&(anchor, _)) = state
        .hits
        .iter()
        .find(|(_, action)| *action == anchor_action)
    else {
        return;
    };
    let choices = menu_actions(state);
    let labels: Vec<_> = choices
        .iter()
        .map(|action| match action {
            Action::Language(index) => state.known[*index].clone(),
            Action::Provider(provider) => format!(
                "{} {}",
                if *provider == state.provider {
                    "✓"
                } else {
                    " "
                },
                provider.label()
            ),
            Action::Mode(mode) => format!("{} {}", if *mode == state.mode { "✓" } else { " " }, mode.label()),
            _ => unreachable!(),
        })
        .collect();
    let labels = if labels.is_empty() {
        vec!["No matching languages".into()]
    } else {
        labels
    };
    let items: Vec<_> = labels.iter().map(|text| DropdownItem::new(text)).collect();
    let visible = anchor
        .bottom()
        .saturating_sub(f.area().y)
        .max(f.area().bottom().saturating_sub(anchor.bottom()))
        .saturating_sub(2)
        .clamp(1, 6);
    let selected = state.menu_index.min(choices.len().saturating_sub(1));
    let scroll = selected.saturating_sub(visible.saturating_sub(1) as usize);
    let area = DropdownMenu::new(
        &items,
        crate::ui_widgets::settings_dropdown_colors(&app.colors),
    )
    .anchor(anchor.x, anchor.bottom())
    .min_width(anchor.width.saturating_sub(2))
    .max_visible(visible)
    .selected(selected)
    .scroll_offset(scroll)
    .mouse(app.mouse_row, app.mouse_col)
    .render(f);
    let state = &mut app.formatter_settings;
    state.menu_index = selected;
    state.popup = Rect::new(
        area.x.saturating_sub(1),
        area.y.saturating_sub(1),
        area.width + 2,
        area.height + 2,
    );
    state.hits.retain(|(rect, _)| !rect.intersects(state.popup));
    for (row, &action) in choices
        .iter()
        .skip(scroll)
        .take(area.height as usize)
        .enumerate()
    {
        state.hits.push((
            Rect::new(area.x, area.y + row as u16, area.width, 1),
            action,
        ));
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    app.formatter_settings.hits.clear();
    app.formatter_settings.popup = Rect::default();
    app.remove_text_input_regions(|id| matches!(id, TextInputId::FormatterSetting(_)));
    let viewport = f.area();
    let width = viewport
        .width
        .saturating_sub(u16::from(viewport.width > 40) * 4)
        .min(84);
    let compact = viewport.height < 16;
    let chrome = if compact { 2 } else { 4 };
    let content = rows(app, width.saturating_sub(4));
    let notice = if let Some(error) = &app.formatter_settings.error {
        error.clone()
    } else if app.formatter_settings.scope == SettingsScope::Workspace
        && local_config(app).is_none()
        && app
            .settings_global
            .formatters
            .as_ref()
            .is_some_and(|map| map.contains_key(&app.formatter_settings.language()))
    {
        "Inherited from User. Save to override for this workspace.".into()
    } else if app.formatter_settings.scope == SettingsScope::Global
        && app
            .settings_workspace
            .formatters
            .as_ref()
            .is_some_and(|map| map.contains_key(&app.formatter_settings.language()))
    {
        "This workspace has an override. Edit Workspace to change it here.".into()
    } else {
        format!(
            "Applies to {} in {}.",
            app.formatter_settings.language(),
            if app.formatter_settings.scope == SettingsScope::Global {
                "all workspaces"
            } else {
                "this workspace"
            }
        )
    };
    let notices =
        App::wrap_setting_text_for_width(&notice, width.saturating_sub(4).max(1) as usize);
    let notice_height = if compact && app.formatter_settings.error.is_none() {
        0
    } else {
        notices.len().min(2) as u16
    };
    let footer_height = 1 + notice_height + u16::from(notice_height > 0 && !compact);
    let height = (2 + chrome + content.len() as u16 + footer_height)
        .min(28)
        .min(
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
    let normal = Style::default()
        .fg(app.colors.ui.widget_text_color(bg))
        .bg(bg);
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .style(normal)
            .border_style(
                Style::default().fg(app
                    .colors
                    .ui
                    .widget_border
                    .border(bg)
                    .unwrap_or(app.colors.border_color)),
            ),
        area,
    );
    if width < 16 || height < 7 {
        return;
    }
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    label(
        f,
        if inner.width < 24 {
            "Formatter"
        } else if app.formatter_settings.request.is_some() {
            "Choose a Formatter"
        } else {
            "Formatter Settings"
        },
        Rect::new(inner.x, inner.y, inner.width.saturating_sub(4), 1),
        normal.add_modifier(Modifier::BOLD),
    );
    button(
        f,
        app,
        Rect::new(inner.right() - 3, inner.y, 3, 1),
        "×",
        Action::Close,
    );
    let mut x = inner.x;
    for (scope, text) in [
        (SettingsScope::Global, "User"),
        (SettingsScope::Workspace, "Workspace"),
    ] {
        let action = Action::Scope(scope);
        let width = (text.len() as u16 + 2).min(inner.right().saturating_sub(x));
        let rect = Rect::new(x, inner.y + if compact { 1 } else { 2 }, width, 1);
        let mut style = app.colors.ui.input_option_style(
            normal.fg.unwrap(),
            bg,
            app.formatter_settings.scope == scope,
            rect.contains((app.mouse_col, app.mouse_row).into()),
        );
        if app.formatter_settings.focus == action {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        label(f, &format!(" {text} "), rect, style);
        if !rect.is_empty() {
            app.formatter_settings.hits.push((rect, action));
        }
        x = rect.right().saturating_add(1).min(inner.right());
    }
    let footer_y = inner.bottom() - 1;
    let body = Rect::new(
        inner.x,
        inner.y + chrome,
        inner.width,
        inner.height.saturating_sub(chrome + footer_height),
    );
    let state = &mut app.formatter_settings;
    let resized = state.body != body;
    state.body = body;
    state.max_scroll = content.len().saturating_sub(body.height as usize);
    if state.reveal_focus || resized {
        if let Some(row) = content
            .iter()
            .position(|row| row_has_focus(row, state.focus))
        {
            state.scroll = state
                .scroll
                .min(row)
                .max((row + 1).saturating_sub(body.height as usize));
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
    if app.formatter_settings.max_scroll > 0 && body.height > 0 {
        let track = Rect::new(area.right() - 2, body.y, 1, body.height);
        let (track_bg, thumb) = app.colors.ui.scrollbar_colors(bg, false, false);
        let pos = (scroll * body.height.saturating_sub(1) as usize
            / app.formatter_settings.max_scroll) as u16;
        label(f, "", track, Style::default().bg(track_bg));
        label(
            f,
            "▐",
            Rect::new(track.x, track.y + pos, 1, 1),
            Style::default().fg(thumb).bg(track_bg),
        );
    }
    for (index, notice) in notices.iter().take(notice_height as usize).enumerate() {
        label(
            f,
            notice,
            Rect::new(
                inner.x,
                body.bottom() + u16::from(!compact) + index as u16,
                inner.width,
                1,
            ),
            normal.fg(if app.formatter_settings.error.is_some() {
                app.colors.ui.error_text_color(bg)
            } else {
                app.colors.ui.description_color(bg)
            }),
        );
    }
    let save_label = if app.formatter_settings.request.is_some()
        && app.formatter_settings.provider != Provider::Disabled
        && inner.width >= 24
    {
        "Save & Format"
    } else {
        "Save"
    };
    let cancel_width = inner.width.min(8);
    let save_width =
        (save_label.len() as u16 + 2).min(inner.width.saturating_sub(cancel_width + 1));
    let cancel = Rect::new(inner.right() - cancel_width, footer_y, cancel_width, 1);
    let save = Rect::new(
        cancel.x.saturating_sub(save_width + 1),
        footer_y,
        save_width,
        1,
    );
    if inner.width >= 38 && app.formatter_settings.request.is_none() && local_config(app).is_some()
    {
        button(
            f,
            app,
            Rect::new(inner.x, footer_y, 7, 1),
            "Reset",
            Action::Reset,
        );
    }
    button(f, app, save, save_label, Action::Save);
    button(f, app, cancel, "Cancel", Action::Close);
    draw_menu(f, app);
}
