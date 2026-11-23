//! Extensions sidebar: one shared set of rendered hit regions for mouse and keyboard navigation.
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{ActivePane, App, SidebarView},
    extensions::{ExtensionRecordView, ExtensionState},
    text_input::TextInputId,
    ui_widgets::InputField,
};

pub(crate) const CARD_HEIGHT: u16 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Install,
    Refresh,
    ReloadAll,
    Clear,
    Filter(&'static str),
    /// Fold or unfold one of [`GROUPS`].
    Collapse(usize),
}

pub(crate) struct Sidebar {
    pub search_focused: bool,
    /// Folded state of each of [`GROUPS`].
    pub collapsed: [bool; GROUPS.len()],
    search_area: Rect,
    pub list_area: Rect,
    pub rows: Vec<(usize, Rect)>,
    pub actions: Vec<(Action, Rect)>,
    last_click: Option<(usize, std::time::Instant)>,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            search_focused: true,
            collapsed: [false; GROUPS.len()],
            search_area: Rect::default(),
            list_area: Rect::default(),
            rows: vec![],
            actions: vec![],
            last_click: None,
        }
    }
}

pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

/// Clip by terminal cells, keeping emoji and combining sequences intact.
pub(crate) fn clip(text: &str, width: u16) -> String {
    let text = clean(text);
    if text.width() <= usize::from(width) {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        if used + grapheme.width() > usize::from(width - 1) {
            break;
        }
        used += grapheme.width();
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

pub(crate) fn publisher(ext: &ExtensionRecordView) -> &str {
    ext.id
        .split_once('.')
        .map_or("Local extension", |(publisher, _)| publisher)
}

pub(crate) fn status(ext: &ExtensionRecordView) -> &'static str {
    match ext.state {
        ExtensionState::Loaded => "Enabled",
        ExtensionState::Error => "Failed",
        ExtensionState::Unloaded => "Disabled",
    }
}

/// Sidebar sections, in display order.
pub(crate) const GROUPS: [(ExtensionState, &str); 3] = [
    (ExtensionState::Loaded, "INSTALLED"),
    (ExtensionState::Unloaded, "DISABLED"),
    (ExtensionState::Error, "ERRORS"),
];

pub(crate) fn group_of(state: ExtensionState) -> usize {
    GROUPS
        .iter()
        .position(|(group, _)| *group == state)
        .unwrap_or(0)
}

/// One row of the sidebar list: a section header or an extension card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Header {
        group: usize,
        matched: usize,
        total: usize,
    },
    Card(usize),
}

impl Item {
    pub(crate) fn height(self) -> u16 {
        match self {
            Item::Header { .. } => 1,
            Item::Card(_) => CARD_HEIGHT,
        }
    }

    pub(crate) fn card(self) -> Option<usize> {
        match self {
            Item::Card(index) => Some(index),
            Item::Header { .. } => None,
        }
    }
}

/// Matching extension indices, grouped by section, including folded ones.
pub(crate) fn grouped(views: &[ExtensionRecordView], query: &str) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..views.len())
        .filter(|index| matches(&views[*index], query))
        .collect();
    indices.sort_by_key(|index| group_of(views[*index].state));
    indices
}

/// The list rows: a header per non-empty section, then its cards unless folded.
pub(crate) fn items(app: &App) -> Vec<Item> {
    let views = app.extension_views();
    let mut items = Vec::new();
    for (group, (state, _)) in GROUPS.iter().enumerate() {
        let total = views.iter().filter(|ext| ext.state == *state).count();
        let cards: Vec<usize> = (0..views.len())
            .filter(|index| {
                views[*index].state == *state && matches(&views[*index], &app.extensions_filter)
            })
            .collect();
        if cards.is_empty() {
            continue;
        }
        items.push(Item::Header {
            group,
            matched: cards.len(),
            total,
        });
        if !app.ext_sidebar.collapsed[group] {
            items.extend(cards.into_iter().map(Item::Card));
        }
    }
    items
}

/// The first item index from which the rest of the list fits in `height`.
pub(crate) fn max_scroll(items: &[Item], height: u16) -> usize {
    if height == 0 {
        return items.len().saturating_sub(1);
    }
    let mut used = 0u16;
    for (position, item) in items.iter().enumerate().rev() {
        used = used.saturating_add(item.height());
        if used > height {
            return position + 1;
        }
    }
    0
}

/// Scroll so the selected card, and its section header when it leads one, is visible.
pub(crate) fn scroll_to_selected(app: &mut App, items: &[Item]) {
    let Some(position) = items
        .iter()
        .position(|item| *item == Item::Card(app.extensions_selected))
    else {
        return;
    };
    let height = app.ext_sidebar.list_area.height;
    let top = if position > 0 && items[position - 1].card().is_none() {
        position - 1
    } else {
        position
    };
    if top < app.extensions_scroll {
        app.extensions_scroll = top;
    }
    while app.extensions_scroll < position
        && items[app.extensions_scroll..=position]
            .iter()
            .map(|item| item.height())
            .sum::<u16>()
            > height
    {
        app.extensions_scroll += 1;
    }
}

pub(crate) fn matches(ext: &ExtensionRecordView, query: &str) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        ext.name, ext.description, ext.id, ext.repository
    )
    .to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| match word {
            "@installed" => true,
            "@enabled" => ext.state == ExtensionState::Loaded,
            "@disabled" => ext.state == ExtensionState::Unloaded,
            "@error" | "@errors" => ext.state == ExtensionState::Error,
            _ => haystack.contains(word),
        })
}

fn button(f: &mut Frame, app: &mut App, area: Rect, label: &str, action: Action, selected: bool) {
    if area.is_empty() {
        return;
    }
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let style = if selected {
        crate::ui_widgets::action_button_style(
            &app.colors,
            app.colors.bg_sidebar,
            true,
            hovered,
            true,
        )
    } else {
        app.colors.ui.toolbar_layer_style(
            app.colors.ui.sidebar_foreground,
            app.colors.bg_sidebar,
            hovered,
            false,
            false,
        )
    };
    f.render_widget(Paragraph::new(clip(label, area.width)).style(style), area);
    app.ext_sidebar.actions.push((action, area));
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    app.ext_sidebar.rows.clear();
    app.ext_sidebar.actions.clear();
    app.ext_sidebar.list_area = Rect::default();
    app.ext_sidebar.search_area = Rect::default();
    app.remove_text_input_regions(|id| id == TextInputId::ExtensionsFilter);
    if area.is_empty() {
        return;
    }
    app.start_empty_window_extension_services();
    let bg = app.colors.bg_sidebar;
    let fg = app.colors.ui.sidebar_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    let base = Style::default().fg(fg).bg(bg);
    f.render_widget(Block::default().style(base), area);
    let inset = u16::from(area.width > 4);
    let inner = Rect::new(
        area.x + inset,
        area.y,
        area.width.saturating_sub(inset * 2),
        area.height,
    );
    let install = if inner.width >= 29 {
        " + Install "
    } else {
        " + "
    };
    let header_buttons = [
        (install, Action::Install),
        (" ↻ ", Action::Refresh),
        (" ⟳ ", Action::ReloadAll),
    ];
    let button_width: u16 = header_buttons
        .iter()
        .map(|(label, _)| label.width() as u16)
        .sum();
    let title_width = inner.width.saturating_sub(button_width + 1);
    f.render_widget(
        Paragraph::new(clip("EXTENSIONS", title_width))
            .style(base.fg(dim).add_modifier(Modifier::BOLD)),
        Rect::new(inner.x, inner.y, title_width, 1),
    );
    let mut x = inner.right().saturating_sub(button_width.min(inner.width));
    for (label, action) in header_buttons {
        let width = (label.width() as u16).min(inner.right().saturating_sub(x));
        button(
            f,
            app,
            Rect::new(x, inner.y, width, 1),
            label,
            action,
            false,
        );
        x += width;
    }
    if inner.height < 2 {
        return;
    }
    let roomy = inner.height >= 15;
    let search_y = inner.y + if roomy { 2 } else { 1 };
    let search_h = if roomy { 3 } else { 1 };
    let search = Rect::new(
        inner.x,
        search_y,
        inner.width,
        search_h.min(inner.bottom().saturating_sub(search_y)),
    );
    let focused = app.ext_sidebar.search_focused
        && app.active_pane == ActivePane::Explorer
        && app.sidebar_view == SidebarView::Extensions;
    app.ext_sidebar.search_area = search;
    let input_style = app.colors.ui.input_style(bg);
    let input_bg = input_style.bg.unwrap_or(bg);
    let border = if focused {
        app.colors.ui.focus_border.border(input_bg)
    } else {
        app.colors.ui.input_colors.border.border(input_bg)
    };
    let block = Block::default()
        .borders(if roomy { Borders::ALL } else { Borders::NONE })
        .style(input_style)
        .border_style(Style::default().fg(border.unwrap_or(input_bg)).bg(input_bg));
    let mut field = block.inner(search);
    f.render_widget(block, search);
    let clear = if !app.extensions_filter.is_empty() && field.width >= 8 {
        let rect = Rect::new(field.right() - 3, field.y, 3, 1);
        field.width -= 3;
        Some(rect)
    } else {
        None
    };
    if !field.is_empty() {
        let hit = InputField::new(&app.extensions_filter)
            .viewport(app.text_input_viewport(TextInputId::ExtensionsFilter))
            .theme_colors(&app.colors)
            .prefix(" ⌕ ", app.colors.ui.icon_color(input_bg))
            .placeholder_text("Search installed extensions")
            .cursor_marker(if focused && app.cursor_visible {
                "│"
            } else {
                ""
            })
            .cursor_pos(app.extensions_filter_cursor)
            .selection_range(App::inline_selection_range(
                app.extensions_filter_cursor,
                app.extensions_filter_selection_anchor,
            ))
            .bg(input_bg)
            .render(f, field);
        app.register_text_input(TextInputId::ExtensionsFilter, hit);
    }
    if let Some(rect) = clear {
        button(f, app, rect, " × ", Action::Clear, false);
    }
    let mut y = search.bottom();
    if inner.height >= 10 && inner.width >= 25 {
        y += u16::from(roomy);
        x = inner.x;
        let filters = if inner.width >= 35 {
            [
                (" All ", ""),
                (" Enabled ", "@enabled"),
                (" Disabled ", "@disabled"),
                (" Errors ", "@error"),
            ]
        } else {
            [
                (" All ", ""),
                (" On ", "@enabled"),
                (" Off ", "@disabled"),
                (" Errors ", "@error"),
            ]
        };
        for (label, token) in filters {
            let width = label.width() as u16;
            let selected = if token.is_empty() {
                !app.extensions_filter
                    .split_whitespace()
                    .any(|w| w.starts_with('@'))
            } else {
                app.extensions_filter
                    .split_whitespace()
                    .any(|w| w.eq_ignore_ascii_case(token))
            };
            button(
                f,
                app,
                Rect::new(x, y, width, 1),
                label,
                Action::Filter(token),
                selected,
            );
            x += width + 1;
        }
        y += 2;
    }
    if y >= inner.bottom() {
        return;
    }
    app.clamp_extensions_selection();
    let views = app.extension_views();
    let any_match = views.iter().any(|ext| matches(ext, &app.extensions_filter));
    let footer_h = u16::from(inner.bottom().saturating_sub(y) >= 7);
    let list = Rect::new(
        inner.x,
        y,
        inner.width,
        inner.bottom().saturating_sub(y + footer_h),
    );
    app.ext_sidebar.list_area = list;
    if !any_match {
        let empty = if views.is_empty() && app.bg_extension_scan_running {
            "Loading extensions…"
        } else if views.is_empty() {
            "Build your workspace

Install an extension from a tagged GitHub release to add tools and language support."
        } else {
            "No extensions found

Try another search or clear your filters."
        };
        f.render_widget(
            Paragraph::new(empty)
                .style(base.fg(dim))
                .wrap(Wrap { trim: true }),
            Rect::new(
                inner.x,
                y.saturating_add(1).min(list.bottom()),
                inner.width,
                list.height.saturating_sub(1),
            ),
        );
        let label = if views.is_empty() {
            " + Install from GitHub "
        } else {
            " Clear search "
        };
        let action = if views.is_empty() {
            Action::Install
        } else {
            Action::Clear
        };
        if list.height >= 7 {
            button(
                f,
                app,
                Rect::new(
                    inner.x,
                    list.bottom() - 2,
                    (label.width() as u16).min(inner.width),
                    1,
                ),
                label,
                action,
                true,
            );
        }
    } else {
        let items = items(app);
        app.extensions_scroll = app.extensions_scroll.min(max_scroll(&items, list.height));
        let scroll = app.extensions_scroll;
        let mut row_y = list.y;
        for item in &items[scroll..] {
            if row_y >= list.bottom() {
                break;
            }
            let height = item.height().min(list.bottom() - row_y);
            match *item {
                Item::Header {
                    group,
                    matched,
                    total,
                } => draw_header(
                    f,
                    app,
                    Rect::new(list.x, row_y, list.width, 1),
                    group,
                    matched,
                    total,
                ),
                Item::Card(index) => {
                    draw_card(
                        f,
                        app,
                        Rect::new(list.x, row_y, list.width, height),
                        index,
                        &views[index],
                        bg,
                        fg,
                    );
                }
            }
            row_y += height;
        }
        let total: u16 = items.iter().map(|item| item.height()).sum();
        if total > list.height && list.width > 0 && list.height > 0 {
            let offset: u16 = items[..scroll].iter().map(|item| item.height()).sum();
            let thumb_h =
                (u32::from(list.height) * u32::from(list.height) / u32::from(total)).max(1) as u16;
            let max_offset: u16 = items[..max_scroll(&items, list.height)]
                .iter()
                .map(|item| item.height())
                .sum();
            let thumb_y = list.y
                + (u32::from(list.height - thumb_h) * u32::from(offset)
                    / u32::from(max_offset.max(1)))
                .min(u32::from(list.height - thumb_h)) as u16;
            let scrollbar_fg = app.colors.ui.description_color(bg);
            f.render_widget(
                Paragraph::new(
                    "▏
"
                    .repeat(usize::from(thumb_h)),
                )
                .style(base.fg(scrollbar_fg)),
                Rect::new(list.right() - 1, thumb_y, 1, thumb_h),
            );
        }
    }
    if footer_h > 0 {
        f.render_widget(
            Paragraph::new(clip("↑↓ Browse  Enter Open  Tab Search", inner.width))
                .style(base.fg(dim)),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}

fn draw_header(
    f: &mut Frame,
    app: &mut App,
    section: Rect,
    group: usize,
    matched: usize,
    total: usize,
) {
    let bg = app.colors.bg_sidebar;
    let dim = app.colors.ui.description_color(bg);
    let style = app
        .colors
        .ui
        .toolbar_layer_style(
            app.colors.ui.section_foreground,
            bg,
            section.contains((app.mouse_col, app.mouse_row).into()),
            false,
            false,
        )
        .add_modifier(Modifier::BOLD);
    let arrow = if app.ext_sidebar.collapsed[group] {
        "›"
    } else {
        "⌄"
    };
    f.render_widget(
        Paragraph::new(clip(&format!("{arrow} {}", GROUPS[group].1), section.width)).style(style),
        section,
    );
    let count = if matched == total {
        format!("{total}")
    } else {
        format!("{matched} / {total}")
    };
    let count_width = count.width() as u16;
    if count_width + 12 <= section.width {
        f.render_widget(
            Paragraph::new(count).style(style.fg(dim).remove_modifier(Modifier::BOLD)),
            Rect::new(section.right() - count_width, section.y, count_width, 1),
        );
    }
    app.ext_sidebar
        .actions
        .push((Action::Collapse(group), section));
}

fn draw_card(
    f: &mut Frame,
    app: &mut App,
    card: Rect,
    index: usize,
    ext: &ExtensionRecordView,
    bg: ratatui::style::Color,
    fg: ratatui::style::Color,
) {
    let selected = index == app.extensions_selected;
    let hovered = card.contains((app.mouse_col, app.mouse_row).into());
    let row_style = app.colors.ui.list_row_style(
        app.colors.ui.sidebar_foreground,
        bg,
        selected,
        app.active_pane == ActivePane::Explorer,
        hovered,
    );
    let style = row_style.remove_modifier(Modifier::UNDERLINED);
    let row_bg = style.bg.unwrap_or(bg);
    let row_fg = style.fg.unwrap_or(fg);
    let secondary = if selected {
        row_fg
    } else {
        app.colors.ui.description_color(row_bg)
    };
    f.render_widget(Block::default().style(style), card);
    let wide = card.width >= 24;
    let text_x = card.x + if wide { 6 } else { 1 };
    let text_width = card.right().saturating_sub(text_x + 2);
    if wide && card.height >= 3 {
        let initial = ext
            .name
            .graphemes(true)
            .find(|g| g.width() == 1 && !g.trim().is_empty())
            .unwrap_or("+");
        let badge = format!("┌─┐\n│{initial}│\n└─┘");
        f.render_widget(
            Paragraph::new(badge).style(style.fg(secondary)),
            Rect::new(card.x + 1, card.y, 3, 3),
        );
    }
    let version = format!("v{}", clean(&ext.version));
    let version_w = if text_width >= 30 {
        (version.width() as u16).min(text_width / 3)
    } else {
        0
    };
    f.render_widget(
        Paragraph::new(clip(
            &ext.name,
            text_width.saturating_sub(version_w + u16::from(version_w > 0)),
        ))
        .style(style.add_modifier(Modifier::BOLD)),
        Rect::new(text_x, card.y, text_width.saturating_sub(version_w), 1),
    );
    if version_w > 0 {
        f.render_widget(
            Paragraph::new(clip(&version, version_w)).style(style.fg(secondary)),
            Rect::new(text_x + text_width - version_w, card.y, version_w, 1),
        );
    }
    if card.height >= 2 {
        let description = if ext.description.is_empty() {
            "No description provided"
        } else {
            &ext.description
        };
        let description_fg = if ext.last_error.is_some() && !selected {
            app.colors.ui.list_error_color(row_bg)
        } else {
            secondary
        };
        f.render_widget(
            Paragraph::new(clip(description, text_width)).style(
                style.fg(app
                    .colors
                    .ui
                    .readable_foreground(description_fg, row_bg, 4.5)),
            ),
            Rect::new(text_x, card.y + 1, text_width, 1),
        );
    }
    if card.height >= 3 {
        let icon = match ext.state {
            ExtensionState::Loaded => "●",
            ExtensionState::Error => "✖",
            ExtensionState::Unloaded => "○",
        };
        let color = match ext.state {
            ExtensionState::Loaded => app.colors.fg_success,
            ExtensionState::Error => app.colors.ui.list_error_color(row_bg),
            ExtensionState::Unloaded => secondary,
        };
        let state = format!("{icon} {}", status(ext));
        let state_w = (state.width() as u16).min(text_width);
        let publisher_w = text_width.saturating_sub(state_w + 2);
        f.render_widget(
            Paragraph::new(clip(publisher(ext), publisher_w)).style(style.fg(secondary)),
            Rect::new(text_x, card.y + 2, publisher_w, 1),
        );
        f.render_widget(
            Paragraph::new(clip(&state, state_w))
                .style(style.fg(app.colors.ui.readable_foreground(color, row_bg, 4.5))),
            Rect::new(text_x + text_width - state_w, card.y + 2, state_w, 1),
        );
    }
    if selected {
        f.render_widget(
            Paragraph::new("▏\n".repeat(usize::from(card.height)))
                .style(style.fg(row_style.underline_color.unwrap_or(row_fg))),
            Rect::new(card.x, card.y, 1, card.height),
        );
    }
    app.ext_sidebar.rows.push((index, card));
}

pub(crate) fn handle_click(app: &mut App, col: u16, row: u16) {
    app.active_pane = ActivePane::Explorer;
    if let Some((action, _)) = app
        .ext_sidebar
        .actions
        .iter()
        .find(|(_, rect)| rect.contains((col, row).into()))
        .copied()
    {
        app.ext_sidebar.last_click = None;
        match action {
            Action::Install => app.open_extension_install_prompt(),
            Action::Refresh => app.extensions_rescan(),
            Action::ReloadAll => app.extensions_reload_all(),
            Action::Clear => {
                app.extensions_filter.clear();
                app.extensions_filter_cursor = 0;
                app.extensions_filter_selection_anchor = None;
                app.extensions_scroll = 0;
                app.ext_sidebar.search_focused = true;
                app.clamp_extensions_selection();
            }
            Action::Filter(token) => {
                let words: Vec<_> = app
                    .extensions_filter
                    .split_whitespace()
                    .filter(|word| {
                        !matches!(
                            word.to_ascii_lowercase().as_str(),
                            "@installed" | "@enabled" | "@disabled" | "@error" | "@errors"
                        )
                    })
                    .collect();
                app.extensions_filter = words
                    .into_iter()
                    .chain((!token.is_empty()).then_some(token))
                    .collect::<Vec<_>>()
                    .join(" ");
                app.extensions_filter_cursor = app.extensions_filter.chars().count();
                app.extensions_filter_selection_anchor = None;
                app.extensions_scroll = 0;
                app.ext_sidebar.collapsed = [false; GROUPS.len()];
                app.clamp_extensions_selection();
            }
            Action::Collapse(group) => {
                app.ext_sidebar.collapsed[group] = !app.ext_sidebar.collapsed[group];
                app.clamp_extensions_selection();
            }
        }
        return;
    }
    if app.ext_sidebar.search_area.contains((col, row).into()) {
        app.ext_sidebar.search_focused = true;
        return;
    }
    if let Some((index, _)) = app
        .ext_sidebar
        .rows
        .iter()
        .find(|(_, rect)| rect.contains((col, row).into()))
        .copied()
    {
        let now = std::time::Instant::now();
        let double = app.ext_sidebar.last_click.is_some_and(|(previous, time)| {
            previous == index && now.duration_since(time).as_millis() < 400
        });
        app.extensions_selected = index;
        app.ext_sidebar.search_focused = false;
        if let Some(ext) = app.extension_views().get(index) {
            if double {
                app.open_extension_info_tab(&ext.id);
            } else {
                app.preview_extension_info_tab(&ext.id);
            }
        }
        app.active_pane = ActivePane::Explorer;
        app.ext_sidebar.last_click = Some((index, now));
    }
}

fn preview_selected(app: &mut App) {
    if let Some(ext) = app
        .extension_views()
        .get(app.extensions_selected)
        .filter(|ext| matches(ext, &app.extensions_filter))
    {
        app.preview_extension_info_tab(&ext.id);
        app.active_pane = ActivePane::Explorer;
    }
}

pub(crate) fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
            app.ext_sidebar.search_focused = false;
            // With every section folded, browsing unfolds them rather than doing nothing.
            if !items(app).iter().any(|item| item.card().is_some()) {
                app.ext_sidebar.collapsed = [false; GROUPS.len()];
            }
            let page = isize::try_from((app.ext_sidebar.list_area.height / CARD_HEIGHT).max(1))
                .unwrap_or(1);
            match code {
                KeyCode::Up => app.extensions_select_prev(),
                KeyCode::Down => app.extensions_select_next(),
                KeyCode::PageUp => app.move_extensions_selection(-page),
                _ => app.move_extensions_selection(page),
            }
            preview_selected(app);
        }
        KeyCode::Home | KeyCode::End if !app.ext_sidebar.search_focused => {
            app.move_extensions_selection(if code == KeyCode::Home {
                isize::MIN
            } else {
                isize::MAX
            });
            preview_selected(app);
        }
        KeyCode::Enter => {
            if let Some(ext) = app
                .extension_views()
                .get(app.extensions_selected)
                .filter(|ext| matches(ext, &app.extensions_filter))
            {
                app.open_extension_info_tab(&ext.id);
            }
        }
        KeyCode::Tab | KeyCode::BackTab => {
            app.ext_sidebar.search_focused = !app.ext_sidebar.search_focused
        }
        KeyCode::Esc => {
            if !app.extensions_filter.is_empty() {
                app.extensions_filter.clear();
                app.extensions_filter_cursor = 0;
                app.extensions_filter_selection_anchor = None;
                app.extensions_scroll = 0;
                app.clamp_extensions_selection();
            }
            app.ext_sidebar.search_focused = true;
        }
        KeyCode::F(5) => app.extensions_rescan(),
        _ => {
            app.ext_sidebar.search_focused = true;
            if app
                .edit_text_input(TextInputId::ExtensionsFilter, code, modifiers, None)
                .changed
            {
                app.text_input_changed(TextInputId::ExtensionsFilter);
            }
        }
    }
}

pub(crate) fn draw_tooltip(f: &mut Frame, app: &App) {
    if !app.sidebar_is_visible() || app.sidebar_view != SidebarView::Extensions {
        return;
    }
    let Some((action, area)) = app
        .ext_sidebar
        .actions
        .iter()
        .find(|(_, rect)| rect.contains((app.mouse_col, app.mouse_row).into()))
    else {
        return;
    };
    let text = match action {
        Action::Install => "Install extension from a tagged GitHub release",
        Action::Refresh => "Rescan installed extensions",
        Action::ReloadAll => "Reload all enabled extensions",
        Action::Clear => "Clear search and filters (Esc)",
        _ => return,
    };
    crate::ui_widgets::Tooltip::hover(vec![Line::from(Span::raw(text))], &app.colors)
        .prefer_below()
        .render(f, area.x, area.y);
}
