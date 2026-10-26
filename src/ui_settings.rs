use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;
use crate::text_input::TextInputId;
use crate::ui_pickers::truncate_to_width;
use crate::ui_widgets::{DropdownMenu, InputField, PopupDialog};

/// Truncate a string to fit within `max_width` display columns, padding with spaces if shorter.
fn truncate_or_pad(s: &str, max_width: usize) -> String {
    let mut result = truncate_to_width(s, max_width);
    let w = UnicodeWidthStr::width(result.as_str());
    if w < max_width {
        result.push_str(&crate::helpers::spaces(max_width - w));
    }
    result
}

fn vscode_setting_label(label: &str, setting_id: &str) -> String {
    if App::setting_field_kind(setting_id) == "heading" {
        return label.trim_matches(['─', ' ']).to_string();
    }
    let prefix = match App::settings_category_for_id(setting_id) {
        1 => "Editor",
        2 => "Workbench",
        3 => "Editor",
        4 => "Files",
        5 => "Keyboard",
        6 => "Agent Chat",
        7 => "Animations",
        8 => "Language Servers",
        _ => "Settings",
    };
    format!("{prefix}: {label}")
}

fn settings_category_pane_width(body_width: u16) -> u16 {
    if body_width < 48 {
        0
    } else {
        (body_width / 5).clamp(22, 30)
    }
}

fn draw_settings_category_dropdown(f: &mut Frame, app: &mut App) {
    if !app.settings_dropdown_open || app.settings_dropdown_setting_id.is_some() {
        return;
    }
    let Some(anchor) = app.settings_category_picker_area else {
        app.close_settings_dropdown();
        return;
    };
    let visible = anchor.bottom().saturating_sub(f.area().y)
        .max(f.area().bottom().saturating_sub(anchor.bottom()))
        .saturating_sub(2)
        .min(app.settings_dropdown_options.len() as u16)
        .min(20);
    // Opening or resizing a short picker must leave its current choice visible.
    // Keep wheel scrolling independent of the highlighted choice otherwise.
    if app.settings_dropdown_area.map(|old| old.height) != Some(visible) {
        let visible = visible.max(1) as usize;
        app.settings_dropdown_scroll = app.settings_dropdown_scroll
            .min(app.settings_dropdown_selected)
            .max(app.settings_dropdown_selected.saturating_sub(visible - 1))
            .min(app.settings_dropdown_options.len().saturating_sub(visible));
    }
    let items: Vec<_> = app.settings_dropdown_options.iter()
        .map(|name| crate::ui_widgets::DropdownItem::new(name))
        .collect();
    let area = DropdownMenu::new(&items, crate::ui_widgets::settings_dropdown_colors(&app.colors))
        .anchor(anchor.x, anchor.bottom())
        .min_width(anchor.width.saturating_sub(2))
        .max_visible(visible)
        .scroll_offset(app.settings_dropdown_scroll)
        .selected(app.settings_dropdown_selected)
        .mouse(app.mouse_row, app.mouse_col)
        .render(f);
    app.settings_dropdown_area = Some(area);
}

pub(crate) fn settings_input_background(color: Color, background: Color, foreground: Color) -> Color {
    // Some light themes rely on a one-pixel input border over a white surface.
    // A terminal field has no such border, so give matching surfaces a light
    // tint to keep empty inputs and unchecked checkboxes visible.
    if color == background {
        crate::app_theme::lerp_color(background, foreground, 0.07)
    } else {
        color
    }
}

pub(crate) fn settings_control_style(
    colors: &crate::app_theme::ThemeColors,
    kind: &str,
    row: Color,
    focused: bool,
) -> Style {
    let control = colors.ui.settings_control(kind);
    let background = control.background.over(row).unwrap_or(row);
    let foreground = control.foreground.over(background).unwrap_or(colors.fg_input);
    let border = control.border.border(background);
    // If a theme supplies a border, represent it with an underline and preserve
    // its exact fill. Only borderless, otherwise invisible fields need a tint.
    let background = if colors.ui.preserve_theme_colors || border.is_some() || control.background.is_transparent() {
        background
    } else {
        settings_input_background(background, row, foreground)
    };
    let mut style = Style::default()
        .bg(background)
        .fg(colors.ui.readable_foreground(
            control.foreground.over(background).unwrap_or(foreground), background, 4.5,
        ));
    // Resolve again after a borderless field's visibility tint changes the fill.
    let border = control.border.border(background);
    let border = if focused {
        colors.ui.focus_border.border(background).or(border)
    } else {
        border
    };
    if let Some(border) = border {
        style = style
            .add_modifier(Modifier::UNDERLINED)
            .underline_color(border);
    }
    style
}

pub(crate) fn underline_field(f: &mut Frame, area: Rect, style: Style) {
    if !style.add_modifier.contains(Modifier::UNDERLINED) {
        return;
    }
    for x in area.x..area.right() {
        if let Some(cell) = f.buffer_mut().cell_mut((x, area.y)) {
            cell.set_style(
                Style::default()
                    .add_modifier(Modifier::UNDERLINED)
                    .underline_color(style.underline_color.unwrap_or(cell.fg)),
            );
        }
    }
}

pub(crate) fn draw_settings_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let items = app.settings_items();
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(app.colors.bg_sidebar)),
        area,
    );
    if area.height == 0 || area.width == 0 {
        app.settings_sidebar_area = None;
        return;
    }

    let title = " Settings";
    let actions = "⋯ ";
    let title_fill = (area.width as usize)
        .saturating_sub(UnicodeWidthStr::width(title) + UnicodeWidthStr::width(actions));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(app.colors.fg_text)
                    .bg(app.colors.bg_sidebar)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                crate::helpers::spaces(title_fill),
                Style::default().bg(app.colors.bg_sidebar),
            ),
            Span::styled(
                actions,
                Style::default()
                    .fg(app.colors.fg_dim)
                    .bg(app.colors.bg_sidebar),
            ),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let inner = Rect::new(
        area.x,
        area.y.saturating_add(2),
        area.width,
        area.height.saturating_sub(2),
    );

    let visible_items = inner.height as usize;
    let scroll_offset = if app.settings_selected >= visible_items {
        app.settings_selected - visible_items + 1
    } else {
        0
    };

    // Store area for mouse click detection
    app.settings_sidebar_area = Some(inner);

    for (vi, idx) in (scroll_offset..items.len()).enumerate() {
        if vi >= visible_items {
            break;
        }
        let (label, value, id) = &items[idx];
        let row_y = inner.y + vi as u16;

        let is_selected = idx == app.settings_selected;
        let is_hovered = app.mouse_row == row_y
            && app.mouse_col >= inner.x
            && app.mouse_col < inner.x + inner.width;
        let bg = if is_selected {
            app.colors.bg_selected
        } else if is_hovered {
            app.colors.bg_tab_inactive
        } else {
            app.colors.bg_sidebar
        };
        let is_heading = App::setting_field_kind(id) == "heading";
        let fg = if is_selected {
            app.colors.fg_selected
        } else if is_heading {
            app.colors.fg_dim
        } else {
            app.colors.fg_text
        };

        let row_area = Rect::new(inner.x, row_y, inner.width, 1);

        let value_text = if is_heading {
            String::new()
        } else if App::setting_field_kind(id) == "checkbox" {
            if value.eq_ignore_ascii_case("on") {
                "☑".to_string()
            } else {
                "☐".to_string()
            }
        } else {
            value.clone()
        };
        let value_width = UnicodeWidthStr::width(value_text.as_str());
        let label_width = (inner.width as usize).saturating_sub(value_width + 2);
        let clean_label = label.trim_matches(['─', ' ']);
        let display_label = if is_heading {
            format!(" {clean_label}")
        } else {
            format!("   {clean_label}")
        };
        let padded_label = truncate_or_pad(&display_label, label_width);

        let row = Paragraph::new(Line::from(vec![
            Span::styled(
                padded_label,
                Style::default().fg(fg).bg(bg).add_modifier(if is_heading {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
            Span::styled(
                value_text,
                Style::default()
                    .fg(
                        if App::setting_field_kind(id) == "checkbox"
                            && value.eq_ignore_ascii_case("on")
                        {
                            app.colors.fg_accent_blue
                        } else {
                            app.colors.fg_dim
                        },
                    )
                    .bg(bg),
            ),
            Span::styled(" ", Style::default().bg(bg)),
        ]));
        f.render_widget(row, row_area);
    }
}

pub(crate) fn draw_settings_editor(f: &mut Frame, app: &mut App, area: Rect) {
    app.theme_management.delete_hits.clear();
    app.settings_dropdown_area = None;
    let bg = app.colors.bg_editor;
    let bg_hover = app.colors.ui.bg_settings_hover;
    let fg_text = app.colors.ui.workbench_text_color(bg);
    let fg_dim = app.colors.ui.description_color(bg);
    let accent = app.colors.fg_accent;

    app.settings_scope_areas = [None, None];
    app.settings_json_area = None;
    app.settings_clear_search_area = None;
    app.settings_category_area = None;
    app.settings_category_picker_area = None;
    app.settings_sidebar_area = None;
    if area.width < 4 || area.height < 4 {
        draw_settings_category_dropdown(f, app);
        return;
    }

    // Fill background
    f.render_widget(Paragraph::new("").style(Style::default().bg(bg)), area);

    // Wide search and scope strip, matching VS Code's Settings editor hierarchy.
    let horizontal_padding = if area.width >= 72 {
        4
    } else if area.width >= 36 {
        2
    } else {
        1
    };
    let search_w = area.width.saturating_sub(horizontal_padding * 2);
    let search_x = area.x.saturating_add(horizontal_padding);
    let searching = !app.settings_editor_search.is_empty();
    let clear_width = if searching && search_w >= 8 { 3 } else { 0 };
    let search_area = Rect::new(
        search_x,
        area.y.saturating_add(1),
        search_w - clear_width,
        1,
    );
    let search_style = settings_control_style(&app.colors, "text", bg,
        !app.settings_focus_categories && !app.settings_focus_control && !app.settings_value_editing && !app.settings_dropdown_open);
    let search_bg = search_style.bg.unwrap_or(bg);
    let search_field = InputField::new(&app.settings_editor_search)
        .theme_control(&app.colors, app.colors.ui.settings_control("text"), bg)
        .prefix(" ", app.colors.ui.description_color(search_bg))
        .placeholder_text("Search settings by name or description")
        .bg(search_bg);
    let search_field = if app.settings_focus_categories
        || app.settings_focus_control
        || app.settings_value_editing
        || app.settings_dropdown_open
    {
        search_field.cursor_marker("")
    } else {
        search_field
            .cursor_marker("█")
            .cursor_pos(app.settings_editor_search_cursor)
            .selection_range(
                App::inline_selection_range(
                    app.settings_editor_search_cursor,
                    app.settings_editor_search_selection_anchor,
                ),
            )
    };
    let hit_map = search_field.render(f, search_area);
    underline_field(f, search_area, search_style);
    app.register_text_input(TextInputId::SettingsSearch, hit_map);

    if clear_width > 0 {
        let clear_area = Rect::new(search_area.right(), search_area.y, clear_width, 1);
        let hovered = clear_area.contains((app.mouse_col, app.mouse_row).into());
        f.render_widget(
            Paragraph::new(" × ").style(app.colors.ui.toolbar_style(
                search_style.fg.unwrap_or(fg_text), search_bg, hovered, false, false,
            )),
            clear_area,
        );
        app.settings_clear_search_area = Some(clear_area);
    }

    let scope_y = area.y.saturating_add(3);
    let global_label = " User ";
    let workspace_label = " Workspace ";
    let global_width = UnicodeWidthStr::width(global_label) as u16;
    let workspace_width = UnicodeWidthStr::width(workspace_label) as u16;
    let global_area = Rect::new(search_x, scope_y, global_width.min(search_w), 1);
    let workspace_x = search_x.saturating_add(global_width).saturating_add(1);
    let workspace_area = Rect::new(
        workspace_x,
        scope_y,
        workspace_width.min(
            search_x
                .saturating_add(search_w)
                .saturating_sub(workspace_x),
        ),
        1,
    );
    app.settings_scope_areas = [Some(global_area), Some(workspace_area)];
    let scope_style = |selected: bool, rect: Rect| {
        let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
        let style = app.colors.ui.toolbar_style(fg_text, bg, hovered, false, selected);
        if selected { style.add_modifier(Modifier::BOLD) } else { style }
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                global_label,
                scope_style(
                    app.settings_scope == crate::settings::SettingsScope::Global,
                    global_area,
                ),
            ),
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                workspace_label,
                scope_style(
                    app.settings_scope == crate::settings::SettingsScope::Workspace,
                    workspace_area,
                ),
            ),
        ])),
        Rect::new(search_x, scope_y, search_w, 1),
    );

    // Keep implementation paths out of the header; the JSON action opens the
    // selected scope's file when it is needed.
    let json_label = if search_w >= 60 {
        " Open Settings (JSON) "
    } else {
        " JSON "
    };
    let json_width = UnicodeWidthStr::width(json_label) as u16;
    if search_w >= global_width + workspace_width + json_width + 4 {
        let json_area = Rect::new(search_x + search_w - json_width, scope_y, json_width, 1);
        let hovered = json_area.contains((app.mouse_col, app.mouse_row).into());
        f.render_widget(
            Paragraph::new(json_label).style(
                app.colors.ui.toolbar_style(app.colors.fg_workbench, bg, hovered, false, false),
            ),
            json_area,
        );
        app.settings_json_area = Some(json_area);
        let hint = "Changes save automatically";
        let hint_width = UnicodeWidthStr::width(hint) as u16;
        if json_area.x.saturating_sub(workspace_area.right()) >= hint_width + 6 {
            f.render_widget(
                Paragraph::new(hint).style(Style::default().fg(fg_dim).bg(bg)),
                Rect::new(json_area.x - hint_width - 3, scope_y, hint_width, 1),
            );
        }
    }

    let header_height = 5;
    let body_area = Rect::new(
        search_x,
        area.y.saturating_add(header_height),
        search_w,
        area.height.saturating_sub(header_height),
    );
    if body_area.width < 8 || body_area.height == 0 {
        app.settings_category_area = None;
        app.settings_sidebar_area = None;
        draw_settings_category_dropdown(f, app);
        return;
    }

    let category_width = settings_category_pane_width(body_area.width);
    if category_width > 0 && app.settings_dropdown_open && app.settings_dropdown_setting_id.is_none() {
        app.close_settings_dropdown();
    }
    let separator_width = u16::from(category_width > 0);
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(category_width),
            Constraint::Length(separator_width),
            Constraint::Min(12),
        ])
        .split(body_area);
    let cat_area = panes[0];
    let separator_area = panes[1];
    let settings_area = panes[2];

    let categories = App::settings_categories();
    if categories.is_empty() {
        app.settings_category_area = None;
        app.settings_sidebar_area = None;
        return;
    }
    if app.settings_category_selected >= categories.len() {
        app.settings_category_selected = categories.len() - 1;
    }

    // A real page tree: independent branches, selected subpages and a separate
    // scroll track keep navigation useful even when many branches are expanded.
    let cat_inner = Rect::new(
        cat_area.x,
        cat_area.y.saturating_add(1),
        cat_area.width,
        cat_area.height.saturating_sub(1),
    );
    if category_width > 0 {
        f.render_widget(Paragraph::new("").style(Style::default().bg(bg)), cat_area);
        for y in separator_area.y..separator_area.y.saturating_add(separator_area.height) {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "│",
                    Style::default()
                        .fg(crate::app_theme::lerp_color(
                            bg,
                            app.colors.border_color,
                            0.4,
                        ))
                        .bg(bg),
                )),
                Rect::new(separator_area.x, y, 1, 1),
            );
        }
        app.settings_category_area = Some(cat_inner);
    } else {
        app.settings_category_area = None;
    }

    let category_rows = app.settings_category_visual_rows();
    app.settings_category_scroll = app.settings_category_scroll
        .min(category_rows.len().saturating_sub(cat_inner.height as usize));
    let has_category_scrollbar = app.settings_category_scrollbar().is_some();
    for (visual_idx, (idx, is_child, name)) in category_rows
        .iter()
        .copied()
        .skip(app.settings_category_scroll)
        .enumerate()
        .take(cat_inner.height as usize)
    {
        if category_width == 0 {
            break;
        }
        let row_y = cat_inner.y + visual_idx as u16;
        let branch_expanded = !app.settings_category_collapsed[idx]
            && !App::settings_sections(idx).is_empty();
        let is_selected = app.settings_editor_search.trim().is_empty()
            && app.settings_tree_row_selected(idx, is_child, name);
        let is_focused = app.settings_focus_categories
            && app.settings_tree_row_selected(idx, is_child, name);
        let is_hovered = app.mouse_row == row_y
            && app.mouse_col >= cat_inner.x
            && app.mouse_col < cat_inner.x + cat_inner.width;
        let row_bg = if is_selected || is_focused {
            app.colors.bg_hover
        } else if is_hovered {
            bg_hover
        } else {
            bg
        };
        let row_fg = app.colors.ui.readable_foreground(if is_selected || is_focused { fg_text } else { fg_dim }, row_bg, 4.5);
        let arrow = if App::settings_sections(idx).is_empty() {
            " "
        } else if branch_expanded {
            "⌄"
        } else {
            "›"
        };
        let marker = if is_selected || is_focused { "▏" } else { " " };
        let mut text = if is_child {
            format!("     {name}")
        } else {
            format!(" {arrow} {name}")
        };
        let max_w = cat_inner.width.saturating_sub(1 + u16::from(has_category_scrollbar)) as usize;
        if UnicodeWidthStr::width(&*text) > max_w {
            text = {
                let mut w = 0;
                text.chars()
                    .take_while(|c| {
                        w += UnicodeWidthChar::width(*c).unwrap_or(0);
                        w <= max_w
                    })
                    .collect()
            };
        }
        let fill = max_w.saturating_sub(UnicodeWidthStr::width(&*text));
        let mut style = if is_selected || is_focused {
            Style::default()
                .fg(row_fg)
                .bg(row_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(row_fg).bg(row_bg)
        };
        if is_focused {
            style = style.add_modifier(Modifier::UNDERLINED)
                .underline_color(app.colors.ui.focus_border.border(row_bg).unwrap_or(accent));
        }
        let row = Line::from(vec![
            Span::styled(
                marker,
                Style::default()
                    .fg(if is_selected || is_focused { accent } else { row_bg })
                    .bg(row_bg),
            ),
            Span::styled(text, style),
            Span::styled(crate::helpers::spaces(fill), Style::default().bg(row_bg)),
        ]);
        f.render_widget(
            Paragraph::new(row),
            Rect::new(cat_inner.x, row_y, cat_inner.width, 1),
        );
    }
    if let Some((height, thumb_height, max_scroll)) = app.settings_category_scrollbar() {
        let top = (height - thumb_height) * app.settings_category_scroll / max_scroll;
        let track_x = cat_inner.right().saturating_sub(1);
        let hovered = app.mouse_col == track_x && cat_inner.contains((app.mouse_col, app.mouse_row).into());
        let (track, thumb) = app.colors.ui.scrollbar_colors(bg, hovered, app.dragging_settings_categories);
        for i in 0..thumb_height {
            f.render_widget(
                Paragraph::new("▐").style(Style::default().fg(thumb).bg(track)),
                Rect::new(track_x, cat_inner.y + (top + i) as u16, 1, 1),
            );
        }
    }

    // Settings pane for selected category
    let selected_category = app
        .settings_category_selected
        .min(categories.len().saturating_sub(1));
    let searching_all_settings = !app.settings_editor_search.trim().is_empty();
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(bg)),
        settings_area,
    );
    let page_heading = if searching_all_settings {
        "Search Results"
    } else {
        categories[selected_category]
    };
    let heading_x = settings_area
        .x
        .saturating_add(if category_width > 0 { 3 } else { 2 });
    let items = app.settings_items();
    let filtered = app.settings_filtered_indices_with_items(selected_category, &items);
    let setting_count = filtered
        .iter()
        .filter(|&&idx| App::setting_field_kind(&items[idx].2) != "heading")
        .count();
    let section = (!searching_all_settings).then(|| app.settings_selected_section(selected_category)).flatten();
    let spacious_header = settings_area.height >= 12;
    let heading = Line::from(vec![
        Span::styled(
            page_heading,
            Style::default()
                .fg(app.colors.ui.settings_header_color(bg))
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "  {setting_count} {}",
                if setting_count == 1 {
                    "setting"
                } else {
                    "settings"
                }
            ),
            Style::default().fg(fg_dim).bg(bg),
        ),
    ]);
    let heading_area = Rect::new(
        heading_x,
        settings_area.y,
        settings_area.right().saturating_sub(heading_x),
        1,
    );
    if category_width == 0 {
        let picker_width = categories.iter()
            .map(|name| UnicodeWidthStr::width(*name) as u16 + 4)
            .max().unwrap_or(4).min(heading_area.width);
        let picker_area = Rect::new(heading_x, settings_area.y, picker_width, 1);
        let style = settings_control_style(&app.colors, "dropdown", bg, app.settings_focus_categories);
        let picker_label = if spacious_header { page_heading } else { section.unwrap_or(page_heading) };
        let label = truncate_or_pad(picker_label, picker_width.saturating_sub(4) as usize);
        f.render_widget(Paragraph::new(format!(" {label} ⌄ ")).style(style), picker_area);
        underline_field(f, picker_area, style);
        app.settings_category_picker_area = Some(picker_area);
        let count = format!("  {setting_count} {}", if setting_count == 1 { "setting" } else { "settings" });
        let count_width = heading_area.width.saturating_sub(picker_width);
        if UnicodeWidthStr::width(count.as_str()) <= count_width as usize {
            f.render_widget(
                Paragraph::new(count).style(Style::default().fg(fg_dim).bg(bg)),
                Rect::new(picker_area.right(), heading_area.y, count_width, 1),
            );
        }
    } else {
        f.render_widget(Paragraph::new(heading), heading_area);
    }
    if spacious_header {
        let subtitle = section.unwrap_or(if searching_all_settings {
            "Matching settings across all categories"
        } else {
            match selected_category {
                0 => "Your most frequently used preferences",
                1 => "Writing, navigating, and formatting code",
                2 => "Appearance and workspace layout",
                3 => "Code assistance, navigation, and search",
                4 => "Saving, file types, and large files",
                5 => "Choose shortcuts for your workflow",
                6 => "Configure your coding agents",
                7 => "Motion and visual effects in the editor",
                8 => "Language intelligence and diagnostics",
                9 => "Git integration and background activity",
                crate::app_settings_ui::SETTINGS_ALL_CATEGORY => "Every setting in one place",
                _ => "",
            }
        });
        let subtitle_style = if section.is_some() {
            Style::default().fg(fg_text).bg(bg).add_modifier(Modifier::BOLD)
        } else { Style::default().fg(fg_dim).bg(bg) };
        f.render_widget(Paragraph::new(subtitle).style(subtitle_style),
            Rect::new(heading_x, settings_area.y + 1, heading_area.width, 1));
        let border = crate::app_theme::lerp_color(bg, app.colors.border_color, 0.35);
        f.render_widget(Paragraph::new("─".repeat(settings_area.width as usize))
            .style(Style::default().fg(border).bg(bg)),
            Rect::new(settings_area.x, settings_area.y + 2, settings_area.width, 1));
    }
    let header_height = if spacious_header { 4 } else { 2 };
    let mut introduction = if section == Some("VTComplete") {
        App::wrap_setting_text_for_width(
            "VTCode's custom autocomplete engine. No language server required.",
            heading_area.width as usize,
        )
    } else {
        Vec::new()
    };
    // Keep room for controls in very short panes.
    if introduction.len() > settings_area.height.saturating_sub(header_height + 3) as usize {
        introduction.clear();
    }
    let introduction_height = introduction.len() as u16;
    if introduction_height > 0 {
        f.render_widget(
            Paragraph::new(introduction.into_iter().map(Line::raw).collect::<Vec<_>>())
                .style(Style::default().fg(fg_dim).bg(bg)),
            Rect::new(heading_x, settings_area.y + header_height - 1, heading_area.width, introduction_height),
        );
    }
    let content_y = settings_area.y.saturating_add(header_height + introduction_height);
    let content_area = Rect::new(
        heading_x.saturating_sub(2),
        content_y,
        settings_area
            .x
            .saturating_add(settings_area.width)
            .saturating_sub(heading_x.saturating_sub(2))
            .saturating_sub(1),
        settings_area
            .y
            .saturating_add(settings_area.height)
            .saturating_sub(content_y),
    );
    app.settings_sidebar_area = Some(content_area);
    app.settings_scroll_areas.push(content_area);
    if content_area.height == 0 || content_area.width == 0 {
        draw_settings_category_dropdown(f, app);
        return;
    }

    if filtered.is_empty() {
        app.settings_editor_scroll = 0;
        app.settings_reveal_selected = false;
        let msg = if !searching_all_settings {
            "No settings in this category"
        } else {
            "No settings found"
        };
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(fg_dim).bg(bg))),
            Rect::new(content_area.x, content_area.y, content_area.width, 1),
        );
        draw_settings_category_dropdown(f, app);
        return;
    }

    let layout = app.settings_layout_cache.get(&items, &filtered, content_area.width.saturating_sub(3) as usize);
    if app.settings_reveal_selected { app.ensure_settings_selection_visible(); }
    let scroll = app.settings_editor_scroll.min(layout.max_scroll(content_area.height as usize));
    app.settings_editor_scroll = scroll;
    let min_y = i64::from(content_area.y);
    let max_y = i64::from(content_area.bottom());
    let mut selected_control_row: Option<u16> = None;
    let mouse_over_dropdown = app.settings_dropdown_open
        && app.settings_dropdown_area.is_some_and(|dd| {
            app.mouse_col >= dd.x
                && app.mouse_col < dd.x + dd.width
                && app.mouse_row >= dd.y
                && app.mouse_row < dd.y + dd.height
        });
    for item in layout.rows.iter().skip(layout.visible_start(scroll)) {
        let item_start_y = min_y + item.start as i64 - scroll as i64;
        if item_start_y >= max_y { break; }
        let mut draw_y = item_start_y;
        let idx = item.index;
        let (label, value, id) = &items[idx];
        let kind = App::setting_field_kind(id);
        let is_heading = kind == "heading";
        let display_label = if searching_all_settings || selected_category == 0 || is_heading {
            vscode_setting_label(label, id)
        } else if section.is_none() && let Some(section) = App::settings_section_for_id(id) {
            format!("{section}: {label}")
        } else {
            label.clone()
        };
        let source_label = app.setting_source_label(id);
        let modified = matches!(source_label, "Global" | "Workspace" | "Project");
        let source_note = if source_label == "Project" || source_label == "Project default" {
            "This project"
        } else if modified {
            "Modified"
        } else if source_label == "Inherited · Global" {
            "Inherited from User"
        } else {
            ""
        };
        let desc_lines = &item.lines;
        let gap = if is_heading { 0 } else { App::setting_row_gap(content_area.width.saturating_sub(3) as usize) };
        let visible_start_y = item_start_y.max(min_y) as u16;
        let item_end_y = (item_start_y + item.height.saturating_sub(gap) as i64).clamp(min_y, max_y) as u16;

        let is_selected = !is_heading && idx == app.settings_selected;
        let is_focused = is_selected
            && !app.settings_focus_categories
            && (app.settings_focus_control
                || app.settings_value_editing
                || app.settings_dropdown_open
                || app.settings_kb_editing);
        let is_hovered = app.mouse_col >= content_area.x
            && app.mouse_col < content_area.x + content_area.width
            && app.mouse_row >= visible_start_y
            && app.mouse_row < item_end_y;
        let is_hovered = !is_heading && is_hovered && !mouse_over_dropdown;
        let row_bg = if is_focused {
            app.colors.ui.bg_settings_focus
        } else if is_hovered {
            bg_hover
        } else {
            bg
        };
        let row_fg = app.colors.ui.readable_foreground(fg_text, row_bg, 4.5);
        let marker_color = if is_focused {
            accent
        } else if modified {
            app.colors.fg_warning
        } else {
            row_bg
        };
        let marker = if is_focused || modified { "▏" } else { " " };
        // Paint the whole setting, including the space above its input, so
        // hover/focus never leaves an unpainted stripe between its controls.
        f.render_widget(Paragraph::new("").style(Style::default().bg(row_bg)),
            Rect::new(content_area.x, visible_start_y, content_area.width.saturating_sub(1), item_end_y.saturating_sub(visible_start_y)));
        if is_focused || modified {
            for y in visible_start_y..item_end_y {
                if let Some(cell) = f.buffer_mut().cell_mut((content_area.x, y)) {
                    cell.set_symbol(marker).set_fg(marker_color).set_bg(row_bg);
                }
            }
        }
        let show_reset = modified
            && !id.starts_with("kb_")
            && (is_hovered || is_focused)
            && content_area.width >= 32;

        let editing_number = matches!(kind, "number" | "language-list")
            && app.settings_value_editing
            && app.settings_value_setting_id.as_deref() == Some(id.as_str());
        let displayed_value = if editing_number {
            app.settings_value_buffer.as_str()
        } else {
            value.as_str()
        };
        let control_text = if id.starts_with("kb_") {
            if is_selected && app.settings_kb_editing {
                "⌨ waiting...".to_string()
            } else {
                value.clone()
            }
        } else if id.starts_with("shortcut_ro_") {
            value.clone()
        } else {
            match kind {
                "checkbox" => {
                    if value.eq_ignore_ascii_case("on") {
                        " ✓ ".to_string()
                    } else {
                        "   ".to_string()
                    }
                }
                "number" | "language-list" | "dropdown" => displayed_value.to_string(),
                _ => value.clone(),
            }
        };

        let control_fg = if id.starts_with("kb_") {
            if is_selected && app.settings_kb_editing {
                app.colors.fg_warning
            } else {
                app.colors.fg_accent_blue
            }
        } else if id.starts_with("shortcut_ro_") {
            app.colors.fg_success
        } else {
            match kind {
                "checkbox" | "number" | "language-list" | "dropdown" => settings_control_style(
                    &app.colors, kind, row_bg, is_focused,
                ).fg.unwrap_or(fg_text),
                "action" => if is_hovered || is_focused { app.colors.fg_selected } else { app.colors.fg_accent_blue },
                _ => fg_dim,
            }
        };

        for (desc_idx, line_text) in desc_lines.iter().enumerate() {
            if draw_y >= max_y {
                break;
            }
            if draw_y < min_y { draw_y += 1; continue; }
            let max_w = content_area.width.saturating_sub(1) as usize;
            let display_text = if is_heading {
                line_text.trim_matches(['─', ' '])
            } else {
                if desc_idx == 0 { display_label.as_str() } else { line_text.as_str() }
            };
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(marker_color).bg(row_bg)),
                Span::styled(" ", Style::default().bg(row_bg)),
            ];
            if desc_idx == 0 {
                let divider_width = if is_heading && id.starts_with("heading_animation_") {
                    4.min(max_w.saturating_sub(3))
                } else {
                    0
                };
                let title_width = max_w.saturating_sub(2 + divider_width + if show_reset { 7 } else { 0 });
                let note_width = UnicodeWidthStr::width(source_note) + 2;
                let has_note = !source_note.is_empty()
                    && UnicodeWidthStr::width(display_text) + note_width <= title_width;
                let title = truncate_to_width(display_text, title_width);
                let title_style = Style::default()
                    .fg(if is_heading {
                        app.colors.ui.settings_header_color(row_bg)
                    } else {
                        row_fg
                    })
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD);
                if (searching_all_settings || selected_category == 0 || section.is_none()) && !is_heading {
                    if let Some((prefix, name)) = title.split_once(": ") {
                        spans.push(Span::styled(
                            format!("{prefix}: "),
                            Style::default().fg(app.colors.ui.description_color(row_bg)).bg(row_bg),
                        ));
                        spans.push(Span::styled(name.to_owned(), title_style));
                    } else {
                        spans.push(Span::styled(title, title_style));
                    }
                } else {
                    spans.push(Span::styled(title, title_style));
                }
                if has_note {
                    spans.push(Span::styled(
                        format!("  {source_note}"),
                        Style::default().fg(app.colors.ui.description_color(row_bg)).bg(row_bg),
                    ));
                }
                if is_heading && id.starts_with("heading_animation_") {
                    let used_width: usize = spans.iter().map(Span::width).sum();
                    let remaining = max_w.saturating_sub(used_width);
                    if remaining > 1 {
                        spans.push(Span::styled(
                            format!(" {}", "─".repeat(remaining - 1)),
                            Style::default().fg(fg_dim).bg(row_bg),
                        ));
                    }
                }
            } else if kind == "checkbox" {
                if desc_idx == 1 {
                    spans.push(Span::styled(
                        control_text.clone(),
                        settings_control_style(&app.colors, "checkbox", row_bg, is_focused),
                    ));
                    spans.push(Span::styled(" ", Style::default().bg(row_bg)));
                } else {
                    spans.push(Span::styled("    ", Style::default().bg(row_bg)));
                }
                spans.push(Span::styled(
                    truncate_to_width(display_text, max_w.saturating_sub(6)),
                    Style::default().fg(app.colors.ui.description_color(row_bg)).bg(row_bg),
                ));
            } else {
                spans.push(Span::styled(
                    truncate_to_width(display_text, max_w.saturating_sub(2)),
                    Style::default().fg(app.colors.ui.description_color(row_bg)).bg(row_bg),
                ));
            }
            f.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default().bg(row_bg)),
                Rect::new(content_area.x, draw_y as u16, max_w as u16, 1),
            );
            if desc_idx == 0 && show_reset {
                let reset_area = Rect::new(content_area.right().saturating_sub(8), draw_y as u16, 7, 1);
                let hovered = reset_area.contains((app.mouse_col, app.mouse_row).into());
                f.render_widget(
                    Paragraph::new(" Reset ").style(
                        app.colors.ui.toolbar_style(app.colors.fg_workbench, row_bg, hovered, false, false),
                    ),
                    reset_area,
                );
            }
            draw_y += 1;
        }

        if is_heading {
            continue;
        }

        let needs_control_row = kind != "checkbox" || desc_lines.len() == 1;
        if needs_control_row {
            draw_y = item_start_y + item.control as i64;
        }
        if draw_y >= min_y && draw_y < max_y && needs_control_row {
            let max_w = content_area.width as usize;
            if id.starts_with("kb_") && max_w > 8 {
                let reset_label = "[Reset]";
                let unassign_label = "[Unassign]";
                let reset_w = UnicodeWidthStr::width(reset_label);
                let unassign_w = UnicodeWidthStr::width(unassign_label);
                let buttons_w = reset_w + 1 + unassign_w;
                let mut left = format!("  {control_text}");
                let left_w = max_w.saturating_sub(buttons_w);
                let left_len = left.chars().count();
                if left_len > left_w {
                    left = left.chars().take(left_w).collect();
                } else if left_len < left_w {
                    left.push_str(&crate::helpers::spaces(left_w - left_len));
                }
                let buttons_x =
                    content_area.x + content_area.width.saturating_sub(buttons_w as u16);
                let hovered_reset = i64::from(app.mouse_row) == draw_y
                    && app.mouse_col >= buttons_x
                    && app.mouse_col < buttons_x + reset_w as u16;
                let unassign_x = buttons_x + reset_w as u16 + 1;
                let hovered_unassign = i64::from(app.mouse_row) == draw_y
                    && app.mouse_col >= unassign_x
                    && app.mouse_col < unassign_x + unassign_w as u16;
                let selected_layer = match app.settings_scope {
                    crate::settings::SettingsScope::Global => &app.settings_global,
                    crate::settings::SettingsScope::Workspace => &app.settings_workspace,
                };
                let reset_enabled = app.setting_is_defined_in_layer(id, selected_layer);
                let unassign_enabled = value != "Unassigned";
                let shortcut_action_style = |enabled: bool, hovered: bool| {
                    let style = app.colors.ui.toolbar_style(app.colors.fg_workbench, row_bg,
                        hovered && enabled, false, false);
                    if enabled { style.add_modifier(Modifier::BOLD) } else {
                        let bg = style.bg.unwrap_or(row_bg);
                        let fg = style.fg.unwrap_or(fg_text);
                        style.fg(crate::ui_colors::disabled_text_color(
                            app.colors.ui.disabled_foreground.over(bg).unwrap_or(fg), fg, bg, app.colors.ui.contrast_colors()))
                    }
                };
                let reset_style = shortcut_action_style(reset_enabled, hovered_reset);
                let unassign_style = shortcut_action_style(unassign_enabled, hovered_unassign);
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(left, Style::default().fg(app.colors.ui.readable_foreground(control_fg, row_bg, 4.5)).bg(row_bg)),
                        Span::styled(reset_label.to_string(), reset_style),
                        Span::styled(" ".to_string(), Style::default().bg(row_bg)),
                        Span::styled(unassign_label.to_string(), unassign_style),
                    ])),
                    Rect::new(content_area.x, draw_y as u16, content_area.width, 1),
                );
            } else {
                let prefix = format!("{marker} ");
                let prefix_w = UnicodeWidthStr::width(prefix.as_str());
                let available_control_width = max_w.saturating_sub(prefix_w + 1);
                let rendered_control = if kind == "dropdown" {
                    let field_width = available_control_width.min(40);
                    if field_width >= 4 {
                        let value_width = field_width.saturating_sub(4);
                        format!(" {} ⌄ ", truncate_or_pad(displayed_value, value_width))
                    } else {
                        truncate_or_pad(displayed_value, field_width)
                    }
                } else if matches!(kind, "number" | "language-list") {
                    let field_width = available_control_width.min(if kind == "number" { 20 } else { 60 });
                    truncate_or_pad(&format!(" {displayed_value}"), field_width)
                } else if kind == "action" {
                    format!(" {} ", control_text.trim())
                } else {
                    control_text
                };
                let control_w = UnicodeWidthStr::width(rendered_control.as_str());
                let fill = max_w.saturating_sub(prefix_w.saturating_add(control_w));
                let control_style = if matches!(kind, "number" | "language-list" | "dropdown" | "checkbox") {
                    settings_control_style(&app.colors, kind, row_bg, is_focused)
                } else if kind == "action" {
                    let style = app.colors.ui.button_style(row_bg, false, is_hovered, true);
                    if is_focused { style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { style }
                } else { Style::default().fg(app.colors.ui.readable_foreground(control_fg, row_bg, 4.5)).bg(row_bg) };
                let control_bg = control_style.bg.unwrap_or(row_bg);
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(marker_color).bg(row_bg)),
                        Span::styled(
                            rendered_control,
                            control_style,
                        ),
                        Span::styled(crate::helpers::spaces(fill), Style::default().bg(row_bg)),
                    ])),
                    Rect::new(content_area.x, draw_y as u16, content_area.width, 1),
                );
                if editing_number {
                    let field_width = available_control_width.min(if kind == "number" { 20 } else { 60 }) as u16;
                    let field_area = Rect::new(
                        content_area.x.saturating_add(prefix_w as u16),
                        draw_y as u16,
                        field_width,
                        1,
                    );
                    let field = InputField::new(&app.settings_value_buffer)
                        .theme_control(&app.colors, app.colors.ui.settings_control(kind), row_bg)
                        .prefix(" ", fg_dim)
                        .bg(control_bg)
                        .cursor_marker("█")
                        .cursor_pos(app.settings_value_cursor)
                        .selection_range(
                            App::inline_selection_range(
                                app.settings_value_cursor,
                                app.settings_value_selection_anchor,
                            ),
                        );
                    let hit_map = field.render(f, field_area);
                    underline_field(f, field_area, control_style);
                    app.register_text_input(TextInputId::SettingsValue, hit_map);
                }
            }
            if is_selected {
                selected_control_row = Some(draw_y as u16);
            }
        }
        if is_focused && let Some(border) = app.colors.ui.settings_row_border.border(row_bg) {
            for y in visible_start_y..item_end_y {
                if let Some(cell) = f.buffer_mut().cell_mut((content_area.x, y)) {
                    cell.set_symbol("▏");
                    cell.set_fg(border);
                    cell.set_bg(row_bg);
                }
            }
        }
    }

    if let Some((track_x, track_h, thumb_h, max_scroll)) =
        layout.scrollbar(content_area)
    {
        let thumb_top = ((track_h.saturating_sub(thumb_h)) * scroll.min(max_scroll)) / max_scroll;
        let hovered = app.mouse_col == track_x
            && app.mouse_row >= content_area.y
            && app.mouse_row < content_area.bottom();
        let (track, thumb) = app.colors.ui.scrollbar_colors(bg, hovered, app.settings_scrollbar_drag.is_some());
        for i in 0..thumb_h {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "▐",
                    Style::default()
                        .fg(thumb)
                        .bg(track),
                )),
                Rect::new(track_x, content_area.y + (thumb_top + i) as u16, 1, 1),
            );
        }
    }

    if app.settings_dropdown_open && app.settings_dropdown_setting_id.is_some()
        && !app.settings_dropdown_options.is_empty()
    {
        let dd_items: Vec<crate::ui_widgets::DropdownItem> = app
            .settings_dropdown_options
            .iter()
            .enumerate()
            .map(|(row, opt)| {
                let item = crate::ui_widgets::DropdownItem::new(opt);
                if app.settings_dropdown_setting_id.as_deref() == Some("theme") {
                    item.shortcut(if app.theme_picker_deletable(row) { "Delete…" } else { "Bundled" })
                } else { item }
            })
            .collect();
        let anchor_x = content_area.x.saturating_add(2);
        let anchor_y = selected_control_row.map_or(content_area.y, |y| y.saturating_add(1));
        let max_vis = content_area.height.saturating_sub(4).clamp(5, 20);
        let colors = crate::ui_widgets::settings_dropdown_colors(&app.colors);
        let dd_area = DropdownMenu::new(&dd_items, colors)
            .anchor(anchor_x, anchor_y)
            .min_width(if app.settings_dropdown_setting_id.as_deref() == Some("theme") { 32 } else { 16 })
            .max_visible(max_vis)
            .scroll_offset(app.settings_dropdown_scroll)
            .selected(app.settings_dropdown_selected)
            .mouse(app.mouse_row, app.mouse_col)
            .render_scrolled(f, &mut app.settings_dropdown_scroll);
        app.settings_dropdown_area = Some(dd_area);
        app.draw_theme_picker_actions(f, dd_area);
    } else if !app.settings_dropdown_open {
        app.settings_dropdown_area = None;
        app.theme_management.delete_hits.clear();
    }
    draw_settings_category_dropdown(f, app);
}

pub(crate) fn draw_settings(f: &mut Frame, app: &App) {
    let area = f.area();
    let items = app.settings_items();
    let max_visible = (area.height.saturating_sub(6)) as usize;
    let height = (max_visible as u16 + 4).min(area.height.saturating_sub(2));

    let inner = PopupDialog::new("⚙ Settings")
        .title_color(app.colors.fg_accent)
        .border_color(app.colors.fg_accent)
        .bg(app.colors.bg_sidebar)
        .size(55, height)
        .render(f);

    let hint_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let hint = Paragraph::new(Line::from(vec![
        Span::styled("↑↓", Style::default().fg(app.colors.fg_accent)),
        Span::styled(" Navigate  ", Style::default().fg(app.colors.fg_dim)),
        Span::styled("Enter", Style::default().fg(app.colors.fg_accent)),
        Span::styled(" Toggle  ", Style::default().fg(app.colors.fg_dim)),
        Span::styled("Esc", Style::default().fg(app.colors.fg_accent)),
        Span::styled(" Close", Style::default().fg(app.colors.fg_dim)),
    ]));
    f.render_widget(hint, hint_area);

    // Scrolling support
    let visible_items = (inner.height as usize).saturating_sub(1);
    let scroll_offset = if app.settings_selected >= visible_items {
        app.settings_selected - visible_items + 1
    } else {
        0
    };

    for (vi, idx) in (scroll_offset..items.len()).enumerate() {
        if vi >= visible_items {
            break;
        }
        let (label, value, _id) = &items[idx];
        let row_y = inner.y + 1 + vi as u16;

        let is_selected = idx == app.settings_selected;
        let bg = if is_selected {
            app.colors.bg_selected
        } else {
            app.colors.bg_sidebar
        };
        let fg = if is_selected {
            app.colors.fg_selected
        } else {
            app.colors.fg_text
        };

        let row_area = Rect::new(inner.x, row_y, inner.width, 1);
        let val_str = format!("[{value}]");
        let val_width = unicode_width::UnicodeWidthStr::width(val_str.as_str()) + 1;
        let label_width = (inner.width as usize).saturating_sub(val_width);
        let padded_label = format!(" {:<width$}", label, width = label_width.saturating_sub(1));

        let row = Paragraph::new(Line::from(vec![
            Span::styled(padded_label, Style::default().fg(fg).bg(bg)),
            Span::styled(val_str, Style::default().fg(app.colors.fg_accent).bg(bg)),
        ]));
        f.render_widget(row, row_area);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        draw_settings_editor, settings_category_pane_width, settings_control_style, truncate_or_pad, vscode_setting_label,
    };
    use crate::app::App;
    use crate::theme_workbench::{ColorLayer, WorkbenchColors};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::{Color, Modifier}};

    #[test]
    fn setting_controls_blend_explicit_and_inherited_roles_on_each_row() {
        let inherited = serde_json::json!({
            "input.background":"#302016c0", "input.foreground":"#fff0bbe0", "input.border":"#ffeedd80",
            "dropdown.background":"#101832c0", "dropdown.foreground":"#ccffdde0", "dropdown.border":"#afddff80",
            "checkbox.background":"#162820c0", "checkbox.foreground":"#eeddffe0", "checkbox.border":"#bbddff80",
            "focusBorder":"#7799ccc0"
        });
        let explicit = serde_json::json!({
            "input.background":"#f0f0f0", "input.foreground":"#101010",
            "dropdown.background":"#eeeeee", "checkbox.background":"#dddddd",
            "settings.checkboxBackground":"#162820c0", "settings.checkboxForeground":"#eeddffe0", "settings.checkboxBorder":"#bbddff80",
            "settings.dropdownBackground":"#101832c0", "settings.dropdownForeground":"#ccffdde0", "settings.dropdownBorder":"#afddff80",
            "settings.numberInputBackground":"#302016c0", "settings.numberInputForeground":"#fff0bbe0", "settings.numberInputBorder":"#ffeedd80",
            "settings.textInputBackground":"#302016c0", "settings.textInputForeground":"#fff0bbe0", "settings.textInputBorder":"#ffeedd80",
            "focusBorder":"#7799ccc0"
        });
        for roles in [inherited, explicit] {
            let mut colors = App::test_new().colors;
            colors.ui = WorkbenchColors::resolve(roles.as_object().unwrap(), &colors, false, false);
            for (kind, fill, text, outline) in [
                ("checkbox", (22, 40, 32), (238, 221, 255), (187, 221, 255)),
                ("dropdown", (16, 24, 50), (204, 255, 221), (175, 221, 255)),
                ("number", (48, 32, 22), (255, 240, 187), (255, 238, 221)),
                ("text", (48, 32, 22), (255, 240, 187), (255, 238, 221)),
            ] {
                for row in [Color::Rgb(12, 20, 28), Color::Rgb(40, 56, 68), Color::Rgb(232, 240, 248)] {
                    let background = ColorLayer::rgba(fill.0, fill.1, fill.2, 192).over(row).unwrap();
                    for focused in [false, true] {
                        let style = settings_control_style(&colors, kind, row, focused);
                        assert_eq!(style.bg, Some(background), "{kind} on {row:?}");
                        assert_eq!(style.fg, ColorLayer::rgba(text.0, text.1, text.2, 224).over(background));
                        assert_eq!(style.underline_color, if focused {
                            ColorLayer::rgba(119, 153, 204, 192).over(background)
                        } else {
                            ColorLayer::rgba(outline.0, outline.1, outline.2, 128).over(background)
                        });
                    }
                }
            }
        }
    }

    #[test]
    fn transparent_settings_fill_and_borders_do_not_gain_a_synthetic_tint() {
        let mut colors = App::test_new().colors;
        let roles = serde_json::json!({
            "settings.textInputBackground":"#0000", "settings.textInputForeground":"#ffffff",
            "settings.textInputBorder":"#0000", "focusBorder":"#0000", "contrastBorder":"#ff0000"
        });
        colors.ui = WorkbenchColors::resolve(roles.as_object().unwrap(), &colors, false, false);
        let row = Color::Rgb(24, 32, 40);
        for focused in [false, true] {
            let style = settings_control_style(&colors, "text", row, focused);
            assert_eq!(style.bg, Some(row));
            assert_eq!(style.fg, Some(Color::Rgb(255, 255, 255)));
            assert!(!style.add_modifier.contains(Modifier::UNDERLINED));
        }
    }

    #[test]
    fn pad_short_string() {
        let r = truncate_or_pad("hi", 5);
        assert_eq!(r, "hi   ");
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn truncate_long_string() {
        let r = truncate_or_pad("hello world", 5);
        assert_eq!(r, "hello");
    }

    #[test]
    fn exact_fit() {
        let r = truncate_or_pad("abc", 3);
        assert_eq!(r, "abc");
    }

    #[test]
    fn empty_string_padded() {
        let r = truncate_or_pad("", 4);
        assert_eq!(r, "    ");
    }

    #[test]
    fn zero_width() {
        let r = truncate_or_pad("hello", 0);
        assert_eq!(r, "");
    }

    #[test]
    fn unicode_chars() {
        let r = truncate_or_pad("你好", 5);
        assert!(r.len() <= 10);
    }

    #[test]
    fn single_char_width() {
        let r = truncate_or_pad("a", 3);
        assert_eq!(r, "a  ");
    }

    #[test]
    fn settings_category_pane_is_responsive() {
        assert_eq!(settings_category_pane_width(40), 0);
        assert_eq!(settings_category_pane_width(47), 0);
        assert_eq!(settings_category_pane_width(48), 22);
        assert_eq!(settings_category_pane_width(120), 24);
        assert_eq!(settings_category_pane_width(240), 30);
    }

    #[test]
    fn setting_labels_use_vscode_section_prefixes() {
        assert_eq!(
            vscode_setting_label("Tab Size", "tab_size"),
            "Editor: Tab Size"
        );
        assert_eq!(
            vscode_setting_label("Color Theme", "theme"),
            "Workbench: Color Theme"
        );
    }

    #[test]
    fn settings_editor_renders_at_desktop_and_tiny_sizes() {
        for (width, height) in [(120, 32), (20, 8)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("terminal");
            let mut app = App::test_new();
            terminal
                .draw(|frame| {
                    draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, height));
                })
                .expect("settings draw");
        }
    }

    #[test]
    fn animation_sections_render_dividers_at_wide_and_narrow_sizes() {
        for (width, height) in [(120, 32), (48, 24), (52, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            let mut app = App::test_new();
            app.settings_category_selected = 7;
            app.ensure_settings_selection_visible();
            let items = app.settings_items();
            let filtered = app.settings_filtered_indices_for_category(7);
            let headings: Vec<_> = filtered.iter().enumerate()
                .filter(|(_, idx)| items[**idx].2.starts_with("heading_animation_"))
                .collect();
            assert_eq!(headings.len(), 5);
            terminal.draw(|frame| draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, height))).unwrap();
            let content = app.settings_sidebar_area.unwrap();
            let layout = app.settings_line_layout(content.width.saturating_sub(3) as usize);
            for (_, &index) in headings {
                let heading_line = layout.row_for_item(index).unwrap().start;
                app.settings_editor_scroll = heading_line;
                terminal.draw(|frame| {
                    draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, height));
                }).expect("animation settings draw");
                let content = app.settings_sidebar_area.expect("settings content");
                let heading_row = (content.left()..content.right())
                    .filter_map(|x| terminal.backend().buffer().cell((x, content.top() + (heading_line - app.settings_editor_scroll) as u16)))
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                let title = items[index].0.trim_matches(['─', ' ']);
                assert!(heading_row.trim_start().starts_with(title.split_whitespace().next().unwrap()), "{heading_row}");
                if width != 52 {
                    assert!(heading_row.contains(title), "{heading_row}");
                }
                assert!(heading_row.contains("──"), "missing divider: {heading_row}");
            }
        }
    }

    #[test]
    fn every_settings_dropdown_opens_with_mouse_and_keyboard_without_cycling() {
        use crossterm::event::{
            KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };

        let app = App::test_new();
        let dropdowns: Vec<_> = app
            .settings_items()
            .into_iter()
            .filter(|(_, _, id)| App::setting_field_kind(id) == "dropdown")
            .collect();
        assert!(dropdowns.iter().any(|(_, _, id)| id == "whitespace"));

        for (label, _, id) in dropdowns {
            let mut app = App::test_new();
            app.settings_editor_active = true;
            app.active_pane = crate::app::ActivePane::Editor;
            app.settings_focus_categories = false;
            app.settings_category_selected = App::settings_category_for_id(&id);
            app.settings_editor_search = label;
            app.settings_selected = app
                .settings_items()
                .iter()
                .position(|(_, _, setting_id)| setting_id == &id)
                .expect("setting");
            app.ensure_settings_selection_visible();
            let before = serde_json::to_value(app.current_settings()).unwrap();
            let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
            terminal
                .draw(|frame| {
                    draw_settings_editor(frame, &mut app, Rect::new(0, 0, 120, 32));
                })
                .unwrap();

            let content = app.settings_sidebar_area.expect("settings content");
            let buffer = terminal.backend().buffer();
            let (column, row) = (content.top()..content.bottom())
                .find_map(|y| {
                    (content.left()..content.right())
                        .find(|&x| buffer.cell((x, y)).is_some_and(|cell| cell.symbol() == "⌄"))
                        .map(|x| (x, y))
                })
                .expect("dropdown arrow");
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
            );
            assert!(app.settings_dropdown_open, "{id}");
            assert_eq!(
                app.settings_dropdown_setting_id.as_deref(),
                Some(id.as_str())
            );
            assert_eq!(
                serde_json::to_value(app.current_settings()).unwrap(),
                before
            );
            terminal
                .draw(|frame| {
                    draw_settings_editor(frame, &mut app, Rect::new(0, 0, 120, 32));
                })
                .unwrap();
            assert!(app.settings_dropdown_area.is_some());

            crate::input_dialogs::handle_settings_editor_key(
                &mut app,
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            );
            assert!(!app.settings_dropdown_open);
            crate::input_dialogs::handle_settings_editor_key(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
            assert!(app.settings_dropdown_open, "{id}");
            assert_eq!(
                serde_json::to_value(app.current_settings()).unwrap(),
                before
            );
        }
    }

    #[test]
    fn settings_editor_registers_clickable_search_caret_geometry() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut app = App::test_new();
        app.settings_editor_active = true;
        app.settings_focus_categories = true;
        app.settings_editor_search = "editor".into();
        app.settings_editor_search_cursor = app.settings_editor_search.len();
        terminal
            .draw(|frame| {
                draw_settings_editor(frame, &mut app, Rect::new(0, 0, 80, 20));
            })
            .expect("settings draw");

        assert_eq!(app.text_input_regions.len(), 1);
        assert!(app.handle_background_text_input_mouse(
            MouseEventKind::Down(MouseButton::Left),
            7,
            1,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.settings_editor_search_cursor, 2);
        assert!(!app.settings_focus_categories);
    }

    #[test]
    fn settings_editor_renders_vscode_hierarchy_and_controls() {
        let width = 140;
        let height = 42;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut app = App::test_new();
        app.settings_editor_active = true;
        app.settings_category_selected = 2;
        app.settings_category_collapsed[2] = false;
        app.settings_selected = app
            .settings_items()
            .iter()
            .position(|(_, _, id)| id == "theme")
            .expect("theme setting");

        terminal
            .draw(|frame| {
                draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, height));
            })
            .expect("settings draw");

        let buffer = terminal.backend().buffer();
        let row = |y| {
            (0..width)
                .filter_map(|x| buffer.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let rendered = (0..height).map(row).collect::<Vec<_>>().join("\n");

        assert!(rendered.contains("Search settings"));
        assert!(rendered.contains("User"));
        assert!(rendered.contains("Workspace"));
        assert!(!rendered.contains("Precedence"));
        assert!(!rendered.contains("Alt+Delete reset"));
        assert!(rendered.contains("Open Settings (JSON)"));
        assert!(!rendered.contains("Applies to every workspace"));
        assert!(!rendered.contains("settings.json"));
        assert!(rendered.contains("⌄ Workbench"));
        assert!(rendered.contains("Appearance"));
        assert!(rendered.contains("Color Theme"));
        assert!(!rendered.contains("Workbench:"));
        assert!(!rendered.contains("Default"));
        assert!(rendered.contains("Dark+"));
        assert!(rendered.contains('✓'));
        assert!(!rendered.contains('☑') && !rendered.contains('☐'));
        assert!(!rendered.contains("[✓]"));
        assert!(!rendered.contains('▶'));
        assert!(!rendered.contains('▼'));
    }

    #[test]
    fn numeric_setting_renders_as_a_real_text_input() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let width = 100;
        let height = 32;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut app = App::test_new();
        app.colors.ui.settings_controls[2].background = crate::theme_workbench::ColorLayer::rgba(11, 22, 33, 255);
        app.colors.ui.settings_controls[2].foreground = crate::theme_workbench::ColorLayer::rgba(224, 225, 226, 255);
        app.settings_editor_active = true;
        app.active_pane = crate::app::ActivePane::Editor;
        app.settings_category_selected = 1;
        app.settings_selected = app
            .settings_items()
            .iter()
            .position(|(_, _, id)| id == "tab_size")
            .expect("tab size setting");

        terminal
            .draw(|frame| {
                draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, height));
            })
            .expect("settings draw");

        let content = app.settings_sidebar_area.expect("settings content");
        let buffer = terminal.backend().buffer();
        let (column, row) = (content.top()..content.bottom())
            .find_map(|y| {
                (content.left()..content.right())
                    .find(|&x| {
                        buffer.cell((x, y)).is_some_and(|cell| {
                            cell.symbol() == "4"
                                && cell.bg == ratatui::style::Color::Rgb(11, 22, 33)
                                && cell.fg == ratatui::style::Color::Rgb(224, 225, 226)
                        })
                    })
                    .map(|x| (x, y))
            })
            .expect("numeric control");
        for _ in 0..2 {
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
            );
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
            );
            app.text_input_regions.clear();
            terminal
                .draw(|frame| {
                    draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, height));
                })
                .expect("settings draw");
        }
        assert_eq!(app.settings_value_selection_anchor, Some(0));
        assert_eq!(app.settings_value_cursor, 1);

        // Search plus the selected numeric value both use the shared editable
        // input control, including selection/caret and paste behavior.
        assert_eq!(app.text_input_regions.len(), 2);
        assert!(app.paste_into_focused_text_input("72invalid"));
        assert_eq!(app.settings_value_buffer, "72");
    }

    #[test]
    fn animation_options_render_and_accept_mouse_and_keyboard_input() {
        use crate::animation_config::AnimationEffect;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut ids = AnimationEffect::ALL
            .into_iter()
            .flat_map(|effect| [effect.duration_id(), effect.intensity_id()])
            .collect::<Vec<_>>();
        ids.extend(["animation_scope_bubble_tight", "animation_scope_bubble_fill", "animation_scope_bubble_disabled_languages"]);
        for width in [46, 100] {
            for &id in &ids {
                let mut app = App::test_new();
                app.settings_editor_active = true;
                app.active_pane = crate::app::ActivePane::Editor;
                app.settings_focus_categories = false;
                app.settings_category_selected = 7;
                app.settings_scope = crate::settings::SettingsScope::Workspace;
                app.settings_editor_search = id.replace('_', " ");
                app.settings_selected = app
                    .settings_items()
                    .iter()
                    .position(|(_, _, candidate)| candidate == id)
                    .unwrap();
                app.ensure_settings_selection_visible();
                let kind = App::setting_field_kind(id);
                let control_index = match kind { "checkbox" => 0, "language-list" => 3, _ => 2 };
                let control_background = ratatui::style::Color::Rgb(11, 22, 33);
                app.colors.ui.settings_controls[control_index].background =
                    crate::theme_workbench::ColorLayer::rgba(11, 22, 33, 255);
                let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
                terminal
                    .draw(|frame| draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, 40)))
                    .unwrap();
                let area = app.settings_sidebar_area.unwrap();
                let (column, row) = (area.top()..area.bottom())
                    .find_map(|y| {
                        (area.left()..area.right())
                            .find(|&x| {
                                terminal
                                    .backend()
                                    .buffer()
                                    .cell((x, y))
                                    .is_some_and(|cell| cell.bg == control_background)
                            })
                            .map(|x| (x, y))
                    })
                    .unwrap_or_else(|| panic!("missing {id} control at width {width}"));
                let checkbox_before = kind == "checkbox"
                    && app.settings_items()[app.settings_selected].1 == "On";
                click(&mut app, column, row);
                let key = App::setting_json_key(id).unwrap();
                if matches!(kind, "number" | "language-list") {
                    assert!(app.settings_value_editing, "{id} at width {width}");
                    assert_eq!(app.settings_value_setting_id.as_deref(), Some(id));
                    app.text_input_regions.clear();
                    terminal
                        .draw(|frame| {
                            draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, 40))
                        })
                        .unwrap();
                    crate::input_dialogs::handle_settings_editor_key(
                        &mut app,
                        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
                    );
                    let value = if kind == "language-list" {
                        "Python, cpp"
                    } else if id.ends_with("_intensity") {
                        "81"
                    } else {
                        "615"
                    };
                    assert!(app.paste_into_focused_text_input(value), "{id}");
                    crate::input_dialogs::handle_settings_editor_key(
                        &mut app,
                        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    );
                    assert!(!app.settings_value_editing);
                    if kind == "language-list" {
                        assert_eq!(serde_json::to_value(&app.settings_workspace).unwrap()[key],
                            serde_json::json!(["cpp", "python"]));
                    } else {
                        assert_eq!(
                            serde_json::to_value(&app.settings_workspace).unwrap()[key].as_u64(),
                            value.parse::<u64>().ok()
                        );
                    }
                    assert!(app.reset_setting_in_selected_scope(id));
                    assert!(
                        serde_json::to_value(&app.settings_workspace)
                            .unwrap()
                            .get(key)
                            .is_none()
                    );
                } else {
                    let mouse_value = serde_json::to_value(&app.settings_workspace).unwrap()[key]
                        .as_bool()
                        .unwrap();
                    assert_eq!(mouse_value, !checkbox_before);
                    crate::input_dialogs::handle_settings_editor_key(
                        &mut app,
                        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                    );
                    assert_eq!(
                        serde_json::to_value(&app.settings_workspace).unwrap()[key].as_bool(),
                        Some(!mouse_value)
                    );
                }
                assert!(
                    !app.animation_settings.any_enabled(),
                    "editing options enabled an effect: {id}"
                );
            }
        }
    }

    #[test]
    fn settings_editor_search_renders_matches_from_other_categories() {
        let width = 140;
        let height = 42;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut app = App::test_new();
        app.settings_editor_active = true;
        app.settings_category_selected = 2; // Workbench
        app.settings_editor_search = "format on save".into();
        app.settings_editor_search_cursor = app.settings_editor_search.len();

        terminal
            .draw(|frame| {
                draw_settings_editor(frame, &mut app, Rect::new(0, 0, width, height));
            })
            .expect("settings draw");

        let buffer = terminal.backend().buffer();
        let rendered = (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Search Results"));
        assert!(rendered.contains("Editor: Format On Save"));
    }

    fn click(app: &mut App, column: u16, row: u16) {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    #[test]
    fn settings_mouse_targets_follow_wrapping_and_leave_titles_and_spacing_inert() {
        for width in [46, 80, 140] {
            let mut app = App::test_new();
            app.settings_editor_active = true;
            app.active_pane = crate::app::ActivePane::Editor;
            app.settings_editor_search = "Show Activity Bar".into();
            app.settings_category_selected = 2;
            app.ensure_settings_selection_visible();
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            terminal
                .draw(|f| draw_settings_editor(f, &mut app, Rect::new(0, 0, width, 40)))
                .unwrap();
            let area = app.settings_sidebar_area.unwrap();
            let before = app.show_activity_bar;
            let height = App::setting_visual_height(
                "Show Activity Bar",
                "activity_bar",
                area.width.saturating_sub(3) as usize,
            );

            click(&mut app, area.x + 3, area.y);
            assert_eq!(app.show_activity_bar, before, "title at {width}");
            click(&mut app, area.x + 3, area.y + height as u16 - 1);
            assert_eq!(app.show_activity_bar, before, "spacing at {width}");

            // Use the last description line, which wraps at the narrower sizes.
            let descriptions = App::setting_description_lines(
                "Show Activity Bar",
                "activity_bar",
                area.width.saturating_sub(3) as usize,
            );
            click(&mut app, area.x + 6, area.y + descriptions.len() as u16 - 1);
            assert_ne!(
                app.show_activity_bar, before,
                "checkbox description at {width}"
            );
            assert_eq!(app.setting_source_label("activity_bar"), "Global");

            if area.width >= 32 {
                app.text_input_regions.clear();
                terminal
                    .draw(|f| draw_settings_editor(f, &mut app, Rect::new(0, 0, width, 40)))
                    .unwrap();
                click(&mut app, area.right() - 5, area.y);
                assert_eq!(app.show_activity_bar, before, "reset at {width}");
                assert_eq!(app.setting_source_label("activity_bar"), "Default");
            }
        }
    }

    #[test]
    fn settings_header_actions_clear_search_and_open_the_selected_scopes_json() {
        for width in [46, 100] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = App::test_new();
            app.settings_global_path = dir.path().join("user.json");
            app.settings_workspace_path = dir.path().join("workspace.json");
            for path in [&app.settings_global_path, &app.settings_workspace_path] {
                std::fs::write(path, "{}").unwrap();
            }
            app.open_settings_tab();
            app.settings_editor_search = "not a real setting".into();
            app.settings_editor_search_cursor = app.settings_editor_search.len();
            app.settings_editor_search_selection_anchor = Some(0);
            let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
            terminal
                .draw(|f| draw_settings_editor(f, &mut app, Rect::new(0, 0, width, 32)))
                .unwrap();
            let clear = app.settings_clear_search_area.unwrap();
            click(&mut app, clear.x + 1, clear.y);
            assert!(app.settings_editor_search.is_empty());
            assert_eq!(app.settings_editor_search_cursor, 0);
            assert_eq!(app.settings_editor_search_selection_anchor, None);
            assert!(!app.settings_focus_control);

            for (scope, path) in [
                (0, app.settings_global_path.clone()),
                (1, app.settings_workspace_path.clone()),
            ] {
                app.open_settings_tab();
                app.text_input_regions.clear();
                terminal
                    .draw(|f| draw_settings_editor(f, &mut app, Rect::new(0, 0, width, 32)))
                    .unwrap();
                let scope_area = app.settings_scope_areas[scope].unwrap();
                click(&mut app, scope_area.x + 1, scope_area.y);
                let json = app.settings_json_area.unwrap();
                click(&mut app, json.x + 1, json.y);
                assert_eq!(
                    app.editor().and_then(|editor| editor.file_path.as_ref()),
                    Some(&path)
                );
            }
        }
    }

    #[test]
    fn settings_keyboard_focus_returns_to_search_without_highlighting_a_setting() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for width in [40, 120] {
            let mut app = App::test_new();
            app.settings_editor_active = true;
            app.active_pane = crate::app::ActivePane::Editor;
            app.settings_editor_search = "Show Activity Bar".into();
            app.ensure_settings_selection_visible();
            let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
            terminal
                .draw(|f| draw_settings_editor(f, &mut app, Rect::new(0, 0, width, 32)))
                .unwrap();
            let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
            crate::input_dialogs::handle_settings_editor_key(&mut app, tab);
            assert!(app.settings_focus_categories);
            crate::input_dialogs::handle_settings_editor_key(&mut app, tab);
            assert!(app.settings_focus_control);
            let before = app.show_activity_bar;
            crate::input_dialogs::handle_settings_editor_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            );
            assert_ne!(app.show_activity_bar, before);
            crate::input_dialogs::handle_settings_editor_key(&mut app, tab);
            assert!(!app.settings_focus_control);
            assert!(!app.settings_focus_categories);
            assert!(
                app.handle_focused_text_input_shortcut(KeyCode::Char('a'), KeyModifiers::CONTROL)
            );
            assert!(app.paste_into_focused_text_input("color theme"));
            assert_eq!(app.settings_editor_search, "color theme");
        }
    }

    fn settings_key(app: &mut App, code: crossterm::event::KeyCode) {
        crate::input_dialogs::handle_settings_editor_key(
            app,
            crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
        );
    }

    #[test]
    fn narrow_settings_picker_reaches_every_category_with_the_mouse() {
        for width in [20, 40, 51] {
            let mut app = App::test_new();
            app.rebuild_keybinding_settings_cache();
            app.open_settings_tab();
            let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
            for (option, (idx, section, name)) in App::settings_page_options().into_iter().enumerate() {
                terminal.draw(|f| draw_settings_editor(f, &mut app, f.area())).unwrap();
                assert!(app.settings_category_area.is_none());
                let picker = app.settings_category_picker_area.expect("compact category selector");
                assert_eq!(terminal.backend().buffer()[(picker.right() - 2, picker.y)].symbol(), "⌄");
                click(&mut app, picker.x, picker.y);
                terminal.draw(|f| draw_settings_editor(f, &mut app, f.area())).unwrap();
                app.settings_dropdown_scroll = option.min(app.settings_dropdown_options.len()
                    .saturating_sub(app.settings_dropdown_area.unwrap().height as usize));
                terminal.draw(|f| draw_settings_editor(f, &mut app, f.area())).unwrap();
                let menu = app.settings_dropdown_area.expect("category options");
                let row = menu.y + (option - app.settings_dropdown_scroll) as u16;
                click(&mut app, menu.x, row);
                assert_eq!(app.settings_category_selected, idx, "{name} at width {width}");
                assert_eq!(app.settings_selected_section(idx), section);
                assert!(!app.settings_dropdown_open);
                assert!(app.settings_focus_categories);
                let filtered = app.settings_filtered_indices_for_category(idx);
                assert!(filtered.contains(&app.settings_selected), "selected control in {name} at width {width}");
                assert_ne!(App::setting_field_kind(&app.settings_items()[app.settings_selected].2), "heading");
            }
        }
    }

    #[test]
    fn narrow_settings_picker_keyboard_selection_cancel_and_focus() {
        use crossterm::event::KeyCode;
        let mut app = App::test_new();
        app.open_settings_tab();
        let mut terminal = Terminal::new(TestBackend::new(40, 32)).unwrap();
        terminal.draw(|f| draw_settings_editor(f, &mut app, f.area())).unwrap();
        settings_key(&mut app, KeyCode::Tab);
        assert!(app.settings_focus_categories);
        settings_key(&mut app, KeyCode::Enter);
        assert!(app.settings_dropdown_open);
        settings_key(&mut app, KeyCode::Down);
        settings_key(&mut app, KeyCode::Esc);
        assert_eq!(app.settings_category_selected, 0);
        assert!(!app.settings_dropdown_open);
        assert!(app.settings_editor_active);

        settings_key(&mut app, KeyCode::Char(' '));
        for _ in 1..App::settings_page_options().len() {
            settings_key(&mut app, KeyCode::Down);
        }
        settings_key(&mut app, KeyCode::Enter);
        assert_eq!(app.settings_category_selected, App::settings_categories().len() - 1);
        assert!(!app.settings_dropdown_open);
        settings_key(&mut app, KeyCode::Tab);
        assert!(app.settings_focus_control);
        settings_key(&mut app, KeyCode::BackTab);
        assert!(app.settings_focus_categories);
        settings_key(&mut app, KeyCode::Down);
        assert!(app.settings_dropdown_open);
        settings_key(&mut app, KeyCode::Tab);
        assert!(!app.settings_dropdown_open);
        assert!(app.settings_focus_control);
        settings_key(&mut app, KeyCode::BackTab);
        settings_key(&mut app, KeyCode::Enter);
        settings_key(&mut app, KeyCode::BackTab);
        assert!(!app.settings_dropdown_open);
        assert_eq!(app.focused_text_input_id(), Some(crate::text_input::TextInputId::SettingsSearch));
    }

    #[test]
    fn narrow_settings_picker_can_leave_empty_search_results() {
        let mut app = App::test_new();
        app.open_settings_tab();
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.settings_editor_search = "no matching setting".into();
        app.settings_editor_search_cursor = app.settings_editor_search.len();
        app.settings_editor_search_selection_anchor = Some(0);
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw_settings_editor(f, &mut app, f.area())).unwrap();
        let picker = app.settings_category_picker_area.unwrap();
        click(&mut app, picker.x, picker.y);
        terminal.draw(|f| draw_settings_editor(f, &mut app, f.area())).unwrap();
        let menu = app.settings_dropdown_area.unwrap();
        click(&mut app, menu.x, menu.y + 4); // Text Editor > Whitespace
        assert_eq!(app.settings_category_selected, 1);
        assert_eq!(app.settings_selected_section(1), Some("Whitespace"));
        assert!(app.settings_editor_search.is_empty());
        assert_eq!(app.settings_editor_search_cursor, 0);
        assert_eq!(app.settings_editor_search_selection_anchor, None);
        assert_eq!(app.settings_scope, crate::settings::SettingsScope::Workspace);
        assert!(app.settings_filtered_indices_for_category(1).contains(&app.settings_selected));
    }

    #[test]
    fn narrow_settings_picker_handles_short_panes_and_resizing() {
        use crossterm::event::KeyCode;
        let mut app = App::test_new();
        app.open_settings_tab();
        app.settings_category_selected = 7;
        app.settings_focus_categories = true;
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        let short = Rect::new(8, 0, 40, 8);
        terminal.draw(|f| draw_settings_editor(f, &mut app, short)).unwrap();
        settings_key(&mut app, KeyCode::Enter);
        terminal.backend_mut().resize(60, 8);
        terminal.autoresize().unwrap();
        terminal.draw(|f| draw_settings_editor(f, &mut app, short)).unwrap();
        let menu = app.settings_dropdown_area.unwrap();
        assert!(menu.height > 0 && menu.height < 8);
        assert!(app.settings_dropdown_selected >= app.settings_dropdown_scroll);
        assert!(app.settings_dropdown_selected < app.settings_dropdown_scroll + menu.height as usize);
        // A short popup overlaps the search field. Its options still own clicks.
        crate::mouse_scroll::handle_scroll_up(&mut app, menu.x, menu.y, crossterm::event::KeyModifiers::NONE);
        terminal.draw(|f| draw_settings_editor(f, &mut app, short)).unwrap();
        let menu = app.settings_dropdown_area.unwrap();
        let expected = app.settings_dropdown_scroll;
        click(&mut app, menu.x, menu.y);
        let (category, section, _) = App::settings_page_options()[expected];
        assert_eq!(app.settings_category_selected, category);
        assert_eq!(app.settings_selected_section(category), section);
        assert!(!app.settings_dropdown_open);

        settings_key(&mut app, KeyCode::Enter);
        assert!(app.settings_dropdown_open);
        terminal.backend_mut().resize(120, 32);
        terminal.autoresize().unwrap();
        terminal.draw(|f| draw_settings_editor(f, &mut app, f.area())).unwrap();
        assert!(app.settings_category_picker_area.is_none());
        assert!(app.settings_category_area.is_some());
        assert!(!app.settings_dropdown_open);
        assert_eq!(app.settings_category_selected, category);
        terminal.draw(|f| draw_settings_editor(f, &mut app, Rect::new(8, 2, 40, 24))).unwrap();
        assert!(app.settings_category_picker_area.is_some());
        assert!(app.settings_category_area.is_none());
        assert_eq!(app.settings_category_selected, category);
    }
}
