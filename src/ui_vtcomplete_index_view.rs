//! Responsive presentation for the file-learning manager. The interaction and
//! bounded inventory state live in the parent module.
use super::*;

fn activity(settings: &IndexSettings, status: &IndexStatus) -> (&'static str, String) {
    match LearningMode::from_settings(settings) {
        LearningMode::Off => (
            "Learning off",
            "Learned data kept. Current-file suggestions still work.".into(),
        ),
        LearningMode::Paused => (
            "Learning paused",
            if matches!(
                status.phase,
                IndexPhase::Indexing | IndexPhase::Scanning | IndexPhase::Loading
            ) {
                "Finishing the current update…".into()
            } else if status.indexed_files == 0 {
                "Start learning to add file suggestions.".into()
            } else {
                "Existing suggestions stay available.".into()
            },
        ),
        LearningMode::On => {
            let detail = match status.phase {
                IndexPhase::Limited => {
                    "Cache full. Opened files replace older entries automatically.".into()
                }
                IndexPhase::Paused | IndexPhase::Disabled | IndexPhase::Cancelled => {
                    "Starting…".into()
                }
                IndexPhase::Idle => "Up to date.".into(),
                IndexPhase::Loading => "Loading saved suggestions…".into(),
                IndexPhase::Scanning => "Discovering workspace files…".into(),
                IndexPhase::Indexing => {
                    if let Some(path) = &status.current_file {
                        format!(
                            "Updating {} · {}%",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            (status.processed_bytes.saturating_mul(100)
                                / status.current_file_bytes.max(1))
                            .min(100)
                        )
                    } else {
                        "Updating file suggestions…".into()
                    }
                }
            };
            ("Learning on", detail)
        }
    }
}

fn text(f: &mut Frame, area: Rect, value: impl Into<String>, style: Style) {
    f.render_widget(Paragraph::new(value.into()).style(style), area);
}

fn ram_usage(bytes: usize) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / 1048576.0)
    }
}

fn button(
    f: &mut Frame,
    app: &App,
    area: Rect,
    label: &str,
    enabled: bool,
    selected: bool,
    bg: Color,
) {
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let mut style = app.colors.ui.list_row_style(
        app.colors.ui.widget_foreground,
        bg,
        selected,
        true,
        hovered && enabled,
    );
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    if !enabled {
        style = style.fg(app.colors.ui.description_color(bg));
    }
    text(f, area, label, style);
}

fn action_button(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    label: &str,
    action: Action,
    enabled: bool,
) -> u16 {
    let width = unicode_width::UnicodeWidthStr::width(label) as u16 + 4;
    if width > area.width || area.is_empty() {
        return 0;
    }
    let rect = Rect::new(area.x, area.y, width, 1);
    crate::ui_widgets::draw_action_button(
        f,
        rect,
        label,
        &app.colors,
        action == Action::ToggleLearning,
        rect.contains((app.mouse_col, app.mouse_row).into()),
        enabled,
    );
    app.completion_index_ui
        .actions
        .push((rect, action, enabled));
    width
}

fn actions(
    f: &mut Frame,
    app: &mut App,
    primary: Rect,
    mut secondary: Rect,
    settings: &IndexSettings,
    status: &IndexStatus,
) {
    let on = LearningMode::from_settings(settings) == LearningMode::On;
    action_button(
        f,
        app,
        primary,
        if on {
            "Pause learning"
        } else {
            "Start learning"
        },
        Action::ToggleLearning,
        true,
    );
    let pending = status.pending_files > 0
        || matches!(
            status.phase,
            IndexPhase::Loading | IndexPhase::Indexing | IndexPhase::Scanning
        );
    // Prefer the action for queued work when a narrow layout can fit only one.
    for (action, label, visible) in [
        (
            Action::Discard,
            if secondary.width >= 19 {
                "Discard updates"
            } else {
                "Discard"
            },
            pending,
        ),
        (Action::Refresh, "Refresh", on),
    ] {
        if visible {
            let width = action_button(f, app, secondary, label, action, true);
            let used = (width + u16::from(width > 0)).min(secondary.width);
            secondary.x += used;
            secondary.width -= used;
        }
    }
}

fn tabs(f: &mut Frame, app: &mut App, area: Rect, status: &IndexStatus, bg: Color) {
    let files = format!(" Files ({}) ", status.indexed_files);
    let files = if files.len() + 20 <= area.width as usize {
        files
    } else {
        " Files ".into()
    };
    text(
        f,
        Rect::new(area.x, area.y + 1, area.width, 1),
        "─".repeat(area.width as usize),
        Style::default()
            .fg(app.colors.ui.description_color(bg))
            .bg(bg),
    );
    let mut x = area.x;
    for (page, label) in [
        (Page::Learning, " Learning "),
        (Page::Limits, " Limits "),
        (Page::Files, files.as_str()),
    ] {
        let width = label.len() as u16;
        if x + width > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        let selected = app.completion_index_ui.page == page;
        let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
        let mut style = app.colors.ui.toolbar_layer_style(
            app.colors.ui.widget_foreground,
            bg,
            hovered,
            false,
            false,
        );
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        } else {
            style = style.fg(app.colors.ui.description_color(bg));
        }
        text(f, rect, label, style);
        if selected {
            let marker = Rect::new(x, area.y + 1, width, 1);
            text(
                f,
                marker,
                "─".repeat(width as usize),
                Style::default()
                    .fg(app.colors.ui.widget_text_color(bg))
                    .bg(bg),
            );
        }
        app.completion_index_ui.tabs.push((rect, page));
        x += width + 1;
    }
}

fn choices(f: &mut Frame, app: &mut App, area: Rect, control: usize, current: &str, bg: Color) {
    let options = app
        .completion_dropdown_options(CHOICE_IDS[control])
        .unwrap();
    if options.len() == 1 {
        text(
            f,
            area,
            current,
            Style::default()
                .fg(app.colors.ui.widget_text_color(bg))
                .bg(bg),
        );
        return;
    }
    let total = options
        .iter()
        .map(|value| value.len() + 4)
        .sum::<usize>()
        .saturating_sub(1);
    if total > area.width as usize && area.width >= 12 {
        let selected = options.iter().position(|value| *value == current).unwrap();
        for (rect, label, option) in [
            (
                Rect::new(area.x, area.y, 3, 1),
                " ‹ ",
                (selected + options.len() - 1) % options.len(),
            ),
            (
                Rect::new(area.right() - 3, area.y, 3, 1),
                " › ",
                (selected + 1) % options.len(),
            ),
        ] {
            button(f, app, rect, label, options.len() > 1, false, bg);
            app.completion_index_ui
                .choices
                .push((rect, control, option));
        }
        text(
            f,
            Rect::new(area.x + 4, area.y, area.width - 8, 1),
            current,
            Style::default()
                .fg(app.colors.ui.widget_text_color(bg))
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        );
        return;
    }
    let mut x = area.x;
    for (option, value) in options.iter().enumerate() {
        let selected = *value == current;
        let label = format!("{} {value} ", if selected { "●" } else { "○" });
        let width = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
        if x + width > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        button(f, app, rect, &label, options.len() > 1, selected, bg);
        app.completion_index_ui
            .choices
            .push((rect, control, option));
        x += width + 1;
    }
}

fn number(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    control: usize,
    current: &str,
    normal: Style,
    bg: Color,
) {
    let suffix = match control {
        3 => "files",
        4 => "MiB per file",
        _ => "MiB RAM",
    };
    let suffix_width = suffix.len() as u16;
    let value_width = area.width.saturating_sub(suffix_width + 9).min(8);
    if value_width < 4 {
        return;
    }
    let back = Rect::new(area.x, area.y, 3, 1);
    let value = Rect::new(back.right() + 1, area.y, value_width, 1);
    let next = Rect::new(value.right() + 1, area.y, 3, 1);
    let units = Rect::new(
        next.right() + 1,
        area.y,
        area.right().saturating_sub(next.right() + 1),
        1,
    );
    text(f, units, suffix, normal);
    let number = current.parse::<u64>().unwrap();
    let (min, max, _) =
        crate::app_vtcomplete_settings::number_range(numeric_id(control).unwrap()).unwrap();
    for (rect, back, label, enabled) in [
        (back, true, " − ", number > min),
        (next, false, " + ", number < max),
    ] {
        button(f, app, rect, label, enabled, false, bg);
        app.completion_index_ui
            .buttons
            .push((rect, control, back, enabled));
    }
    app.completion_index_ui.values.push((value, control));
    if let Some(edit) = app
        .completion_index_ui
        .editing
        .as_mut()
        .filter(|edit| edit.control == control)
    {
        edit.hit_map = Some(
            crate::ui_widgets::InputField::new(&edit.text)
                .theme_control(&app.colors, app.colors.ui.input_colors, bg)
                .prefix("", normal.fg.unwrap_or(Color::Reset))
                .cursor_pos(edit.cursor)
                .selection_range(App::inline_selection_range(edit.cursor, edit.anchor))
                .render(f, value),
        );
    } else {
        crate::ui_widgets::InputField::new(current)
            .theme_control(&app.colors, app.colors.ui.input_colors, bg)
            .prefix("", normal.fg.unwrap_or(Color::Reset))
            .render(f, value);
    }
}

fn controls(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    settings: &IndexSettings,
    normal: Style,
    dim: Color,
    bg: Color,
) {
    let row_height = if area.width >= 60 && area.height >= 9 {
        3
    } else {
        2
    };
    let count = (area.height / row_height).min(3) as usize;
    if count == 0 {
        return;
    }
    let rows = control_rows(settings);
    let state = &mut app.completion_index_ui;
    let first = state.page.first();
    state.selected = state.selected.clamp(first, first + 2);
    state.scroll = state
        .scroll
        .min(state.selected - first)
        .max((state.selected - first).saturating_sub(count - 1));
    let start = first + state.scroll;
    for (offset, control) in (start..first + 3).take(count).enumerate() {
        let y = area.y + offset as u16 * row_height;
        let label = Rect::new(area.x, y, area.width, 1);
        let selected = control == app.completion_index_ui.selected;
        let style = if selected {
            normal.add_modifier(Modifier::BOLD)
        } else {
            normal.fg(dim)
        };
        text(
            f,
            label,
            format!("{} {}", if selected { "›" } else { " " }, rows[control].0),
            style,
        );
        if selected && count < 3 && area.width >= 28 {
            text(
                f,
                Rect::new(label.right() - 6, label.y, 6, 1),
                format!("{}/3", control - first + 1),
                normal.fg(dim),
            );
        }
        app.completion_index_ui.controls.push((label, control));
        let input = Rect::new(area.x + 2, y + 1, area.width.saturating_sub(2), 1);
        if control < 3 {
            choices(f, app, input, control, &rows[control].1, bg);
        } else {
            number(f, app, input, control, &rows[control].1, normal, bg);
        }
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let status = app.completion_index.status();
    let settings = app.completion_index.settings();
    draw_snapshot(f, app, status, settings);
}

pub(super) fn draw_snapshot(
    f: &mut Frame,
    app: &mut App,
    status: IndexStatus,
    settings: IndexSettings,
) {
    if !app.completion_index_ui.open {
        return;
    }
    let viewport = f.area();
    let width = viewport.width.saturating_sub(2).min(84);
    let height = viewport.height.saturating_sub(2).min(24);
    let area = Rect::new(
        viewport.x + (viewport.width - width) / 2,
        viewport.y + (viewport.height - height) / 2,
        width,
        height,
    );
    app.completion_index_ui.area = area;
    app.completion_index_ui.clear_targets();
    if width < 8 || height < 4 {
        return;
    }
    let bg = crate::ui_widgets::widget_background(f, &app.colors, area);
    let fg = app.colors.ui.widget_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    let normal = Style::default().fg(fg).bg(bg);
    let title = if width < 35 {
        " VTComplete "
    } else {
        " VTComplete learning "
    };
    let block = Block::default()
        .title(title)
        .title_style(normal.add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(normal)
        .border_style(Style::default().fg(app.colors.ui.widget_border.border(bg).unwrap_or(bg)));
    let mut inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let close = Rect::new(area.right() - 4, area.y, 3, 1);
    button(f, app, close, " × ", true, false, bg);
    app.completion_index_ui.close = close;
    if inner.height < 10 || inner.width < 32 {
        if app.completion_index_ui.page == Page::Files {
            let pane = Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(1),
            );
            draw_files(f, app, pane, normal, dim, bg);
        } else if inner.height >= 3 {
            let selected = app.completion_index_ui.selected;
            let rows = control_rows(&settings);
            text(
                f,
                Rect::new(inner.x, inner.y, inner.width, 1),
                clip_middle(&rows[selected].0, inner.width as usize),
                normal,
            );
            text(
                f,
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
                clip_middle(&rows[selected].1, inner.width as usize),
                normal.fg(dim),
            );
        }
        if inner.height > 0 {
            text(
                f,
                Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
                " Esc close",
                normal.fg(dim),
            );
        }
        return;
    }
    let padding = if inner.width >= 60 { 2 } else { 1 };
    inner.x += padding;
    inner.width = inner.width.saturating_sub(padding * 2);
    let wide = inner.width >= 60 && inner.height >= 20;
    let mode = LearningMode::from_settings(&settings);
    let (title, detail) = activity(&settings, &status);
    let top = inner.y;
    let primary_width = 18;
    let (primary, secondary, status_width, stats_y, tabs_y, body_y) = if wide {
        (
            Rect::new(inner.right() - primary_width, top, primary_width, 1),
            Rect::new(inner.right() - 31, top + 3, 31, 1),
            inner.width - primary_width - 2,
            top + 3,
            top + 5,
            top + 8,
        )
    } else {
        (
            Rect::new(inner.x, top + 1, primary_width, 1),
            Rect::new(
                inner.x + primary_width + 1,
                top + 1,
                inner.width.saturating_sub(primary_width + 1),
                1,
            ),
            inner.width,
            top + 2,
            top + 3,
            top + 5,
        )
    };
    let needs_attention = status.error.is_some() || status.phase == IndexPhase::Limited;
    let marker = match mode {
        LearningMode::On | LearningMode::Paused => "●",
        LearningMode::Off => "○",
    };
    let state_color = if mode == LearningMode::Off {
        dim
    } else if needs_attention || mode == LearningMode::Paused {
        app.colors.ui.warning_text_color(bg)
    } else {
        app.colors
            .ui
            .readable_foreground(app.colors.fg_success, bg, 4.5)
    };
    let attention = if status.error.is_some() {
        if status_width >= title.len() as u16 + 20 {
            " · needs attention"
        } else {
            " · error"
        }
    } else if status.phase == IndexPhase::Limited && mode == LearningMode::On {
        " · cache full"
    } else {
        ""
    };
    text(
        f,
        Rect::new(inner.x, top, status_width, 1),
        format!("{marker} {title}{attention}"),
        normal.fg(state_color).add_modifier(Modifier::BOLD),
    );
    if wide && status.error.is_none() {
        text(
            f,
            Rect::new(inner.x, top + 1, inner.width, 1),
            clip_middle(&detail, inner.width as usize),
            normal.fg(dim),
        );
    }
    actions(f, app, primary, secondary, &settings, &status);
    let stats_width = if wide {
        app.completion_index_ui
            .actions
            .iter()
            .filter(|(rect, action, _)| rect.y == stats_y && *action != Action::ToggleLearning)
            .map(|(rect, _, _)| rect.x.saturating_sub(inner.x + 1))
            .min()
            .unwrap_or(inner.width)
    } else {
        inner.width
    };
    let mut stats = format!("{} files", status.indexed_files);
    for part in [
        (status.pending_files > 0).then(|| format!("{} pending", status.pending_files)),
        (status.skipped_files > 0).then(|| format!("{} skipped", status.skipped_files)),
        (status.memory_bytes > 0).then(|| format!("{} RAM", ram_usage(status.memory_bytes))),
    ]
    .into_iter()
    .flatten()
    {
        if unicode_width::UnicodeWidthStr::width(stats.as_str()) + 3 + part.len()
            <= stats_width as usize
        {
            stats.push_str(" · ");
            stats.push_str(&part);
        }
    }
    text(
        f,
        Rect::new(inner.x, stats_y, stats_width, 1),
        stats,
        normal.fg(dim),
    );
    tabs(
        f,
        app,
        Rect::new(inner.x, tabs_y, inner.width, 1),
        &status,
        bg,
    );
    let help_height = 2;
    let footer_y = inner.bottom() - 1;
    let help_y = footer_y - help_height;
    let body = Rect::new(inner.x, body_y, inner.width, help_y.saturating_sub(body_y));
    if app.completion_index_ui.page == Page::Files {
        draw_files(f, app, body, normal, dim, bg);
    } else {
        controls(f, app, body, &settings, normal, dim, bg);
    }
    let state = &app.completion_index_ui;
    let contextual_help =
        if app.remote_workspace.is_some() && state.page == Page::Learning && state.selected == 1 {
            Some("Uses loaded remote files. No extra SSH reads.")
        } else if !wide
            && state.page == Page::Learning
            && state.selected == 0
            && mode == LearningMode::Paused
            && status.indexed_files == 0
        {
            Some(detail.as_str())
        } else if status.phase == IndexPhase::Limited
            && state.page == Page::Learning
            && mode == LearningMode::On
            && !wide
        {
            Some("Opened files replace older cache entries automatically.")
        } else if !wide
            && mode == LearningMode::On
            && matches!(
                status.phase,
                IndexPhase::Loading | IndexPhase::Indexing | IndexPhase::Scanning
            )
        {
            Some(detail.as_str())
        } else {
            None
        };
    let help = state
        .edit_error
        .as_deref()
        .or(status.error.as_deref())
        .or(state.file_message.as_deref())
        .or(contextual_help)
        .unwrap_or_else(|| {
            if state.page == Page::Files {
                if state.file_total() == 0 {
                    ""
                } else {
                    "Removes learned data only. Source files stay."
                }
            } else {
                control_help(state.selected, &settings)
            }
        });
    f.render_widget(
        Paragraph::new(help)
            .style(
                normal.fg(if state.edit_error.is_some() || status.error.is_some() {
                    app.colors.ui.warning_text_color(bg)
                } else {
                    dim
                }),
            )
            .wrap(Wrap { trim: true }),
        Rect::new(inner.x, help_y, inner.width, help_height),
    );
    let footer = if state.editing.is_some() {
        "Enter save · Esc cancel"
    } else {
        "Esc close"
    };
    let footer_width = unicode_width::UnicodeWidthStr::width(footer) as u16;
    text(
        f,
        Rect::new(inner.right() - footer_width, footer_y, footer_width, 1),
        footer,
        normal.fg(dim),
    );
    let link = "All settings";
    if inner.width >= link.len() as u16 + footer_width + 2 {
        app.completion_index_ui.settings_button =
            Rect::new(inner.x, footer_y, link.len() as u16, 1);
        let rect = app.completion_index_ui.settings_button;
        let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
        text(
            f,
            rect,
            link,
            app.colors
                .ui
                .toolbar_layer_style(app.colors.ui.widget_foreground, bg, hovered, false, false)
                .add_modifier(Modifier::UNDERLINED),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_explains_modes_without_disabling_all_vtcomplete() {
        let mut settings = IndexSettings::default();
        settings.paused = true;
        let mut status = IndexStatus::default();
        status.phase = IndexPhase::Paused;
        assert_eq!(activity(&settings, &status).0, "Learning paused");
        assert!(activity(&settings, &status).1.contains("Start learning"));
        settings.enabled = false;
        let (title, detail) = activity(&settings, &status);
        assert_eq!(title, "Learning off");
        assert!(detail.contains("Current-file suggestions"));
        settings.enabled = true;
        settings.paused = false;
        assert_eq!(activity(&settings, &status).0, "Learning on");
        assert_eq!(activity(&settings, &status).1, "Starting…");
        status.phase = IndexPhase::Limited;
        assert!(
            activity(&settings, &status)
                .1
                .contains("replace older entries automatically")
        );
    }
}
