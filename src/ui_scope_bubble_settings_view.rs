use super::*;
use crate::ui_widgets::{DropdownItem, DropdownMenu, InputField};
use ratatui::{
    Frame,
    layout::Margin,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

enum Row {
    Gap,
    Hint(String),
    Header,
    Rule(usize),
    Control(Action),
    Notice(String),
}
fn rows(app: &App, width: u16) -> Vec<Row> {
    let state = &app.scope_bubble_settings;
    let mut rows = Vec::new();
    let hint = |rows: &mut Vec<Row>, text: &str| {
        rows.extend(
            App::wrap_setting_text_for_width(text, width.max(1) as usize)
                .into_iter()
                .map(Row::Hint),
        )
    };
    if !state.editing {
        hint(
            &mut rows,
            "Override built-in scopes or add a custom language. Select a row to edit.",
        );
        rows.push(Row::Gap);
        if state.rules.is_empty() {
            hint(&mut rows, "Using automatic language rules.");
        } else {
            rows.push(Row::Header);
            rows.extend((0..state.rules.len()).map(Row::Rule));
        }
        rows.extend([Row::Gap, Row::Control(Action::New)]);
    } else {
        rows.push(Row::Control(Action::Language));
        hint(
            &mut rows,
            "Language name or ID. Choose an installed syntax, or type a custom name.",
        );
        rows.extend([Row::Gap, Row::Control(Action::ChooseSyntax)]);
        hint(
            &mut rows,
            "Automatic recognizes registered C-style grammars. Choose a style to override scope parsing, including comments, strings, and blocks.",
        );
        if state.draft.syntax != Syntax::Disabled {
            rows.push(Row::Gap);
            if state.syntax().has_delimiters() {
                hint(&mut rows, "Delimiter scopes");
                rows.extend(Delimiter::ALL.map(|pair| Row::Control(Action::Pair(pair))));
            }
            rows.extend([Row::Gap, Row::Control(Action::Blocks)]);
            hint(
                &mut rows,
                "Include the style's built-in blocks, such as Python indentation or C preprocessor branches.",
            );
        }
        if state.scope == SettingsScope::Workspace && state.original.is_some() && !state.can_reset()
        {
            rows.push(Row::Gap);
            hint(
                &mut rows,
                "Inherited from User. Save creates a workspace override.",
            );
        } else if state.scope == SettingsScope::Global
            && app
                .settings_workspace
                .animation_scope_bubble_languages
                .as_ref()
                .is_some_and(|map| {
                    map.keys().any(|name| {
                        config::language_key(name) == config::language_key(&state.field.text)
                    })
                })
        {
            rows.push(Row::Gap);
            hint(
                &mut rows,
                "This workspace overrides this language. Edit Workspace to change it here.",
            );
        }
        if width < 26 && state.can_reset() {
            rows.extend([Row::Gap, Row::Control(Action::Reset)]);
        }
    }
    if let Some(error) = &state.error {
        rows.push(Row::Gap);
        rows.extend(
            App::wrap_setting_text_for_width(error, width.max(1) as usize)
                .into_iter()
                .map(Row::Notice),
        );
    }
    rows
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
        matches!(action, Action::Save | Action::New),
        area.contains((app.mouse_col, app.mouse_row).into()),
        true,
    );
    if app.scope_bubble_settings.focus == action {
        style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    }
    label(f, &format!(" {text} "), area, style);
    app.scope_bubble_settings.hits.push((area, action));
}
fn columns(area: Rect) -> [Rect; 3] {
    let lang = area.width * 2 / 5;
    let syntax = area.width / 3;
    [
        Rect::new(area.x, area.y, lang.saturating_sub(1), 1),
        Rect::new(area.x + lang, area.y, syntax.saturating_sub(1), 1),
        Rect::new(
            area.x + lang + syntax,
            area.y,
            area.width - lang - syntax,
            1,
        ),
    ]
}
fn draw_row(f: &mut Frame, app: &mut App, row: &Row, area: Rect, bg: Color) {
    let normal = Style::default()
        .fg(app.colors.ui.widget_text_color(bg))
        .bg(bg);
    let subdued = normal.fg(app.colors.ui.description_color(bg));
    match row {
        Row::Gap => {}
        Row::Hint(text) => label(f, text, area, subdued),
        Row::Notice(text) => label(f, text, area, normal.add_modifier(Modifier::BOLD)),
        Row::Header => {
            for (area, text) in columns(area)
                .into_iter()
                .zip(["Language", "Syntax", "Scopes"])
            {
                label(f, text, area, normal.add_modifier(Modifier::BOLD));
            }
        }
        Row::Rule(index) => {
            let (name, rule, local) = &app.scope_bubble_settings.rules[*index];
            let action = Action::Rule(*index);
            let style = app.colors.ui.input_option_style(
                normal.fg.unwrap(),
                bg,
                app.scope_bubble_settings.focus == action,
                area.contains((app.mouse_col, app.mouse_row).into()),
            );
            label(f, "", area, style);
            let [language, syntax, scopes] = columns(area);
            let name = if *local {
                name.clone()
            } else {
                format!("{name} (User)")
            };
            label(f, &name, language, style);
            label(f, rule.syntax.label(), syntax, style);
            let pairs = match rule.delimiters.as_ref() {
                Some(pairs) => pairs
                    .iter()
                    .map(|pair| match pair {
                        Delimiter::Braces => "{}",
                        Delimiter::Parentheses => "()",
                        Delimiter::Brackets => "[]",
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                None => "Default".into(),
            };
            let summary = if rule.syntax == Syntax::Disabled {
                "Off".into()
            } else if rule.language_blocks {
                format!("{pairs} + blocks")
            } else if pairs.is_empty() {
                "None".into()
            } else {
                pairs
            };
            label(f, &summary, scopes, style);
            app.scope_bubble_settings.hits.push((area, action));
        }
        Row::Control(Action::New) => button(
            f,
            app,
            Rect {
                width: area.width.min(16),
                ..area
            },
            "Add Language",
            Action::New,
        ),
        Row::Control(Action::Reset) => button(
            f,
            app,
            Rect {
                width: area.width.min(14),
                ..area
            },
            "Reset rule",
            Action::Reset,
        ),
        Row::Control(action @ (Action::Language | Action::ChooseSyntax)) => {
            let caption_width = (area.width / 3).min(15);
            label(
                f,
                if *action == Action::Language {
                    "Language"
                } else {
                    "Syntax style"
                },
                Rect {
                    width: caption_width,
                    ..area
                },
                normal,
            );
            let control = Rect::new(
                area.x + caption_width,
                area.y,
                area.width - caption_width,
                1,
            );
            let focused = app.scope_bubble_settings.focus == *action;
            let style = crate::ui_settings::settings_control_style(
                &app.colors,
                if *action == Action::Language {
                    "text"
                } else {
                    "dropdown"
                },
                bg,
                focused,
            );
            if *action == Action::Language {
                let input = Rect {
                    width: control.width.saturating_sub(3),
                    ..control
                };
                let field = &app.scope_bubble_settings.field;
                let hit = InputField::new(&field.text)
                    .theme_control(&app.colors, app.colors.ui.settings_control("text"), bg)
                    .bg(style.bg.unwrap_or(bg))
                    .prefix("", Color::Reset)
                    .placeholder_text("e.g. C++ or My Language")
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
                crate::ui_settings::underline_field(f, input, style);
                app.register_text_input(TextInputId::ScopeBubbleLanguage, hit);
                app.scope_bubble_settings
                    .hits
                    .push((input, Action::Language));
                button(
                    f,
                    app,
                    Rect::new(input.right(), area.y, control.width - input.width, 1),
                    "⌄",
                    Action::ChooseLanguage,
                );
            } else {
                label(f, "", control, style);
                label(
                    f,
                    &format!(" {}", app.scope_bubble_settings.draft.syntax.label()),
                    Rect {
                        width: control.width.saturating_sub(2),
                        ..control
                    },
                    style,
                );
                if control.width >= 2 {
                    label(
                        f,
                        "⌄",
                        Rect::new(control.right() - 2, control.y, 2, 1),
                        style,
                    );
                }
                crate::ui_settings::underline_field(f, control, style);
                app.scope_bubble_settings.hits.push((control, *action));
            }
        }
        Row::Control(action @ (Action::Pair(_) | Action::Blocks)) => {
            let state = &app.scope_bubble_settings;
            let (checked, text) = match action {
                Action::Pair(pair) => (state.pairs().contains(pair), pair.label()),
                _ => (state.draft.language_blocks, "Language blocks"),
            };
            let style = app.colors.ui.input_option_style(
                normal.fg.unwrap(),
                bg,
                state.focus == *action,
                area.contains((app.mouse_col, app.mouse_row).into()),
            );
            label(
                f,
                &format!("{}  {text}", if checked { "[✓]" } else { "[ ]" }),
                area,
                style,
            );
            app.scope_bubble_settings.hits.push((area, *action));
        }
        Row::Control(_) => {}
    }
}
fn draw_menu(f: &mut Frame, app: &mut App) {
    let state = &app.scope_bubble_settings;
    let Some(menu) = state.menu else {
        return;
    };
    let Some((anchor, _)) = state.hits.iter().find(|(_, action)| *action == menu) else {
        return;
    };
    let anchor = *anchor;
    let choices = state.choices();
    let labels: Vec<String> = choices
        .iter()
        .map(|action| match action {
            Action::PickLanguage(i) => state.languages[*i].clone(),
            Action::PickSyntax(syntax) => format!(
                "{} {}",
                if *syntax == state.draft.syntax {
                    "✓"
                } else {
                    " "
                },
                syntax.label()
            ),
            _ => unreachable!(),
        })
        .collect();
    let labels = if labels.is_empty() {
        vec!["No matches — type a custom name".into()]
    } else {
        labels
    };
    let items: Vec<_> = labels.iter().map(|text| DropdownItem::new(text)).collect();
    let visible = anchor
        .bottom()
        .saturating_sub(f.area().y)
        .max(f.area().bottom().saturating_sub(anchor.bottom()))
        .saturating_sub(2)
        .clamp(1, 8);
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
    let state = &mut app.scope_bubble_settings;
    state.menu_index = selected;
    state.popup = Rect::new(
        area.x.saturating_sub(1),
        area.y.saturating_sub(1),
        area.width.saturating_add(2),
        area.height.saturating_add(2),
    );
    state.hits.retain(|(rect, _)| !rect.intersects(state.popup));
    app.remove_text_input_regions(|id| id == TextInputId::ScopeBubbleLanguage);
    let state = &mut app.scope_bubble_settings;
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
pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    app.scope_bubble_settings.hits.clear();
    app.scope_bubble_settings.popup = Rect::default();
    app.remove_text_input_regions(|id| id == TextInputId::ScopeBubbleLanguage);
    let viewport = f.area();
    let width = viewport
        .width
        .saturating_sub(u16::from(viewport.width > 40) * 4)
        .min(86);
    let content = rows(app, width.saturating_sub(5));
    let height = (content.len() + 8).min(30).min(
        viewport
            .height
            .saturating_sub(u16::from(viewport.height > 12) * 2) as usize,
    ) as u16;
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
    if width < 20 || height < 7 {
        return;
    }
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    label(
        f,
        "Scope Bubble · Languages",
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
        let rect = Rect::new(
            x,
            inner.y + 1,
            (text.len() as u16 + 2).min(inner.right().saturating_sub(x)),
            1,
        );
        let style = app.colors.ui.input_option_style(
            normal.fg.unwrap(),
            bg,
            app.scope_bubble_settings.scope == scope,
            !app.scope_bubble_settings.editing
                && rect.contains((app.mouse_col, app.mouse_row).into()),
        );
        label(
            f,
            &format!(" {text} "),
            rect,
            if app.scope_bubble_settings.focus == Action::Scope(scope) {
                style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                style
            },
        );
        if !app.scope_bubble_settings.editing && !rect.is_empty() {
            app.scope_bubble_settings
                .hits
                .push((rect, Action::Scope(scope)));
        }
        x = rect.right().saturating_add(1).min(inner.right());
    }
    let body = Rect::new(
        inner.x,
        inner.y + 3,
        inner.width.saturating_sub(1),
        inner.height.saturating_sub(4),
    );
    let state = &mut app.scope_bubble_settings;
    let resized = state.body != body;
    state.body = body;
    state.max_scroll = content.len().saturating_sub(body.height as usize);
    if state.reveal_focus || resized {
        let target = if state.error.is_some() {
            content.iter().position(|row| matches!(row, Row::Notice(_)))
        } else {
            content.iter().position(|row| match row {
                Row::Control(action) => {
                    *action == state.focus
                        || (*action == Action::Language && state.focus == Action::ChooseLanguage)
                }
                Row::Rule(i) => state.focus == Action::Rule(*i),
                _ => false,
            })
        };
        if let Some(row) = target {
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
    if app.scope_bubble_settings.max_scroll > 0 && body.height > 0 {
        let track = Rect::new(inner.right() - 1, body.y, 1, body.height);
        let (track_bg, thumb) = app.colors.ui.scrollbar_colors(bg, false, false);
        let pos = (scroll * body.height.saturating_sub(1) as usize
            / app.scope_bubble_settings.max_scroll) as u16;
        label(f, "", track, Style::default().bg(track_bg));
        label(
            f,
            "▐",
            Rect::new(track.x, track.y + pos, 1, 1),
            Style::default().fg(thumb).bg(track_bg),
        );
    }
    let actions = if app.scope_bubble_settings.editing {
        let mut actions = vec![("Save", Action::Save), ("Cancel", Action::Cancel)];
        if app.scope_bubble_settings.can_reset() && body.width >= 26 {
            actions.push(("Reset", Action::Reset));
        }
        actions
    } else {
        vec![("Done", Action::Close)]
    };
    let mut x = inner.x;
    for (text, action) in actions {
        let rect = Rect::new(
            x,
            inner.bottom() - 1,
            (text.len() as u16 + 2).min(inner.right().saturating_sub(x)),
            1,
        );
        button(f, app, rect, text, action);
        x = rect.right().saturating_add(1).min(inner.right());
    }
    draw_menu(f, app);
}
